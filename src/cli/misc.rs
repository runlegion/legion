//! Small one-call dispatch arms: init, now, surface, whoami, status, work,
//! daemon control, statusline and friends (carved from main.rs, #610).

use clap::Subcommand;

use crate::cli::datadir::data_dir;
use crate::cli::memory::try_load_embed_model;
use crate::cli::util::{open_db, open_db_and_index};
use crate::{
    daemon, error, identity_generate, init, kanban, mcp, now, recall, serve, stats, status,
    statusline, surface, task, watch, worksource,
};

#[derive(Subcommand)]
pub(crate) enum TaskAction {
    /// Create a new task for another agent
    Create {
        /// Sender repository name
        #[arg(long)]
        from: String,

        /// Target repository name
        #[arg(long)]
        to: String,

        /// Task description
        #[arg(long)]
        text: String,

        /// Additional context for the task
        #[arg(long)]
        context: Option<String>,

        /// Priority: low, med, high (default: med)
        #[arg(long, default_value = "med", value_parser = ["low", "med", "high"])]
        priority: String,
    },

    /// List tasks for a repo
    List {
        /// Repository name
        #[arg(long)]
        repo: String,

        /// Show outbound tasks (tasks created by this repo) instead of inbound
        #[arg(long)]
        from: bool,
    },

    /// Accept a pending task
    Accept {
        /// Task ID
        #[arg(long)]
        id: String,
    },

    /// Mark an accepted task as done
    Done {
        /// Task ID
        #[arg(long)]
        id: String,

        /// Completion note
        #[arg(long)]
        note: Option<String>,
    },

    /// Block an accepted task
    Block {
        /// Task ID
        #[arg(long)]
        id: String,

        /// Reason for blocking
        #[arg(long)]
        reason: Option<String>,
    },

    /// Unblock a blocked task (returns to accepted)
    Unblock {
        /// Task ID
        #[arg(long)]
        id: String,
    },
}

pub(crate) fn handle_mcp_health(wait: u64) -> error::Result<()> {
    // Spawn `legion mcp` as a subprocess. Send `initialize`, wait
    // for the notifier to tick at least once, then send
    // `legion/notifier_health`. Print the result JSON. Exit
    // non-zero only on hard transport failure (binary missing,
    // stdio closed); a `stale` or `unknown` health verdict is
    // still a successful PROBE and prints to stdout for the
    // operator to interpret.
    let exe = std::env::current_exe()
        .map_err(|e| error::LegionError::Server(format!("current_exe: {e}")))?;
    let mut child = std::process::Command::new(exe)
        .arg("mcp")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| error::LegionError::Server(format!("spawn legion mcp: {e}")))?;
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| error::LegionError::Server("mcp stdin missing".into()))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| error::LegionError::Server("mcp stdout missing".into()))?;
    let mut reader = std::io::BufReader::new(stdout);

    use std::io::{BufRead, Write};
    let init = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"legion-mcp-health","version":"0"}}}"#;
    writeln!(stdin, "{init}").ok();
    // Drain the init response so subsequent reads land on the
    // health response.
    let mut buf = String::new();
    reader.read_line(&mut buf).ok();

    // Give the notifier time to tick at least once.
    std::thread::sleep(std::time::Duration::from_secs(wait));

    let health_req = r#"{"jsonrpc":"2.0","id":2,"method":"legion/notifier_health"}"#;
    writeln!(stdin, "{health_req}").ok();
    buf.clear();
    reader
        .read_line(&mut buf)
        .map_err(|e| error::LegionError::Server(format!("read mcp response: {e}")))?;

    let _ = child.kill();
    let _ = child.wait();

    // Parse and pretty-print the result field; fall back to raw
    // line on parse failure so the operator sees the actual bytes.
    match serde_json::from_str::<serde_json::Value>(buf.trim()) {
        Ok(v) => {
            let result = v.get("result").cloned().unwrap_or(v);
            println!("{}", serde_json::to_string_pretty(&result)?);
        }
        Err(_) => print!("{buf}"),
    }
    Ok(())
}

pub(crate) fn handle_now(json: bool) -> error::Result<()> {
    // The default and `--banner` output are the same one-line
    // string; `banner` is accepted for explicitness in scripts
    // (matches `legion index --status --banner`'s convention).
    let snap = now::NowSnapshot::from_local(chrono::Local::now());
    if json {
        println!("{}", serde_json::to_string(&snap)?);
    } else {
        println!("{}", snap.banner());
    }
    Ok(())
}

pub(crate) fn handle_init(force: bool) -> error::Result<()> {
    init::init(force)?;
    Ok(())
}

pub(crate) fn handle_surface(
    repo: String,
    since: Option<String>,
    until: Option<String>,
    on: Option<String>,
) -> error::Result<()> {
    let database = open_db()?;
    let range =
        crate::timerange::TimeRange::parse(since.as_deref(), until.as_deref(), on.as_deref())?;

    let result = surface::surface(&database, &repo, &range)?;
    let output = surface::format_surface(&result, &repo);
    if !output.is_empty() {
        print!("{output}");
    }
    Ok(())
}

pub(crate) fn handle_whoami(repo: String, limit: usize) -> error::Result<()> {
    let database = open_db()?;
    let roots = database.get_identity_roots(&repo, limit)?;
    if roots.is_empty() {
        return Ok(());
    }
    let mut entries = Vec::with_capacity(roots.len());
    for r in roots {
        let in_chain = database.is_in_chain(&r.id)?;
        entries.push(recall::WhoamiEntry {
            id: r.id,
            text: r.text,
            in_chain,
        });
    }
    let output = recall::format_whoami(&repo, &entries);
    print!("{output}");
    Ok(())
}

/// `legion whoami --generate`: gather claimed-half + given-half source
/// material and print it as JSON for the calling agent to synthesize into
/// an `IdentityManifest` by hand. Flag validation (both `--vault-repo` and
/// a non-empty `--byline` required) returns `LegionError::WhoamiGenerate`
/// naming the missing flag(s), per this command's error-handling contract.
pub(crate) fn handle_whoami_generate(
    repo: String,
    vault_repo: Option<String>,
    byline: Vec<String>,
) -> error::Result<()> {
    let vault_repo = vault_repo.ok_or_else(|| {
        error::LegionError::WhoamiGenerate(
            "--vault-repo is required with --generate (unless --apply is also set)".to_string(),
        )
    })?;
    if byline.is_empty() {
        return Err(error::LegionError::WhoamiGenerate(
            "at least one --byline is required with --generate (unless --apply is also set)"
                .to_string(),
        ));
    }

    let (database, index) = open_db_and_index()?;
    let embed_model = try_load_embed_model();
    let bundle = identity_generate::gather(
        &database,
        &index,
        embed_model.as_ref(),
        &repo,
        &vault_repo,
        &byline,
    )?;
    println!("{}", serde_json::to_string_pretty(&bundle)?);
    Ok(())
}

/// `legion whoami --generate --apply`: read an authored `IdentityManifest`
/// from `--from-file` and perform the guarded identity-chain swap (or, with
/// `--dry-run`, report the plan without writing or deleting anything).
pub(crate) fn handle_whoami_apply(
    repo: String,
    from_file: Option<std::path::PathBuf>,
    dry_run: bool,
) -> error::Result<()> {
    let from_file = from_file.ok_or_else(|| {
        error::LegionError::WhoamiGenerate("--from-file is required with --apply".to_string())
    })?;
    let manifest_text = std::fs::read_to_string(&from_file)?;
    let manifest: identity_generate::IdentityManifest = serde_json::from_str(&manifest_text)?;

    let (database, index) = open_db_and_index()?;
    let backup_dir = data_dir()?.join("identity-backups");
    let result =
        identity_generate::apply(&database, &index, &repo, &manifest, &backup_dir, dry_run)?;
    println!("{}", serde_json::to_string_pretty(&result)?);
    Ok(())
}

pub(crate) fn handle_whatami(repo: String, limit: usize) -> error::Result<()> {
    let database = open_db()?;
    let roots = database.get_domain_roots(&repo, "workflow", limit)?;
    if roots.is_empty() {
        return Ok(());
    }
    let mut entries = Vec::with_capacity(roots.len());
    for r in roots {
        let in_chain = database.is_in_chain(&r.id)?;
        entries.push(recall::WhoamiEntry {
            id: r.id,
            text: r.text,
            in_chain,
        });
    }
    let output = recall::format_whatami(&repo, &entries);
    print!("{output}");
    Ok(())
}

pub(crate) fn handle_stats(repo: Option<String>) -> error::Result<()> {
    let database = open_db()?;
    stats::stats(&database, repo.as_deref())?;
    Ok(())
}

pub(crate) fn handle_serve(port: u16) -> error::Result<()> {
    let base = data_dir()?;
    let watch_path = base.join("watch.toml");
    if watch_path.exists() {
        let contents = std::fs::read_to_string(&watch_path)?;
        let config: watch::WatchConfig = toml::from_str(&contents)?;
        if !config.serve {
            return Err(error::LegionError::Server(
                "serve is not enabled on this node. Set serve = true in watch.toml to designate this machine as the dashboard server.".to_string(),
            ));
        }
    }
    serve::run_server(port, base)?;
    Ok(())
}

pub(crate) fn handle_status(repo: String, json: bool) -> error::Result<()> {
    let database = open_db()?;
    let output = status::get_status(&database, &repo)?;
    if json {
        println!(
            "{}",
            serde_json::to_string(&status::format_summary(&output))?
        );
    } else {
        let formatted = status::format_status(&output);
        if formatted.is_empty() {
            println!(
                "[Legion] Status for {}: all clear. Check `gh issue list` for GitHub issues.",
                repo
            );
        } else {
            print!("{formatted}");
        }
    }
    Ok(())
}

pub(crate) fn handle_needs(repo: String) -> error::Result<()> {
    let database = open_db()?;
    let items = status::get_needs(&database, &repo)?;
    print!("{}", status::format_needs(&repo, &items));
    Ok(())
}

pub(crate) fn handle_work(repo: String, peek: bool) -> error::Result<()> {
    let database = open_db()?;

    // Sync from external work sources before checking the queue
    if let Some((plugin, source_repo, workdir)) = worksource::resolve_config(&repo) {
        match worksource::sync_issues(&database, &plugin, &source_repo, &workdir, &repo) {
            Ok(n) if n > 0 => info!("[legion] synced {n} new issues from {plugin}"),
            Ok(_) => {}
            Err(e) => eprintln!("[legion] work source sync failed: {e}"),
        }
    }

    let card = if peek {
        kanban::peek_work(&database, &repo)?
    } else {
        kanban::next_work(&database, &repo)?
    };

    match card {
        Some(c) => print!("{}", kanban::format_work_card(&c)),
        None => info!("[legion] no pending work for {repo}"),
    }
    Ok(())
}

pub(crate) fn handle_sync(repo: String) -> error::Result<()> {
    let database = open_db()?;

    // Sync from external work sources. Unlike `legion work`, an
    // unconfigured repo is a hard error here: syncing is the whole
    // point of the command.
    let (plugin, source_repo, workdir) = worksource::require_worksource(&repo)?;
    let synced = worksource::sync_issues(&database, &plugin, &source_repo, &workdir, &repo)?;
    if synced > 0 {
        info!("[legion] synced {synced} new issues from {plugin}");
    }
    Ok(())
}

pub(crate) fn handle_task(action: TaskAction) -> error::Result<()> {
    let database = open_db()?;

    match action {
        TaskAction::Create {
            from,
            to,
            text,
            context,
            priority,
        } => {
            let id =
                task::create_task(&database, &from, &to, &text, context.as_deref(), &priority)?;
            println!("{id}");
            info!("[legion] task created: {} -> {}", from, to);
        }
        TaskAction::List { repo, from } => {
            let direction = if from {
                task::Direction::Outbound
            } else {
                task::Direction::Inbound
            };
            let tasks = task::list_tasks(&database, &repo, direction)?;
            let output = task::format_task_list(&tasks, &repo, direction);
            if output.is_empty() {
                info!("[legion] no tasks found");
            } else {
                print!("{output}");
            }
        }
        TaskAction::Accept { id } => {
            task::accept_task(&database, &id)?;
            info!("[legion] task accepted: {}", id);
        }
        TaskAction::Done { id, note } => {
            task::complete_task(&database, &id, note.as_deref())?;
            info!("[legion] task completed: {}", id);
        }
        TaskAction::Block { id, reason } => {
            task::block_task(&database, &id, reason.as_deref())?;
            info!("[legion] task blocked: {}", id);
        }
        TaskAction::Unblock { id } => {
            task::unblock_task(&database, &id)?;
            info!("[legion] task unblocked: {}", id);
        }
    }
    Ok(())
}

pub(crate) fn handle_mcp() -> error::Result<()> {
    let base = data_dir()?;
    let version = env!("CARGO_PKG_VERSION").to_string();
    let (tx, _rx) = tokio::sync::broadcast::channel(16);
    mcp::run_stdio_loop(base, version, tx)?;
    Ok(())
}

pub(crate) fn handle_mcp_logs(pid: Option<u32>, tail: bool) -> error::Result<()> {
    let path = match pid {
        Some(p) => mcp::mcp_log_path(p)?,
        None => match mcp::most_recent_mcp_log()? {
            Some(p) => p,
            None => {
                eprintln!(
                    "[legion] no MCP log files found at {}",
                    mcp::mcp_log_dir()?.display()
                );
                return Ok(());
            }
        },
    };
    if !path.exists() {
        eprintln!("[legion] log file does not exist: {}", path.display());
        return Ok(());
    }
    if tail {
        let status = std::process::Command::new("tail")
            .args(["-F", "-n", "+1"])
            .arg(&path)
            .status()
            .map_err(error::LegionError::Io)?;
        if !status.success() {
            return Err(error::LegionError::WorkSource(format!(
                "tail exited with status {status}"
            )));
        }
    } else {
        let contents = std::fs::read_to_string(&path)?;
        print!("{contents}");
    }
    Ok(())
}

pub(crate) fn handle_daemon(port: u16) -> error::Result<()> {
    let base = data_dir()?;
    daemon::run_daemon(daemon::DaemonConfig {
        data_dir: base,
        port,
        enable_mcp: false,
    })?;
    Ok(())
}

pub(crate) fn handle_daemon_spawn(port: u16) -> error::Result<()> {
    let base = data_dir()?;
    daemon::spawn_detached(&base, port)?;
    Ok(())
}

pub(crate) fn handle_daemon_stop() -> error::Result<()> {
    let base = data_dir()?;
    if daemon::stop_detached(&base)? {
        eprintln!("[legion] daemon stopped");
    } else {
        eprintln!("[legion] no running daemon to stop");
    }
    Ok(())
}

pub(crate) fn handle_daemon_restart(port: u16) -> error::Result<()> {
    let base = data_dir()?;
    daemon::restart_detached(&base, port)?;
    Ok(())
}

pub(crate) fn handle_goal(repo: String) -> error::Result<()> {
    let database = open_db()?;
    let cards = kanban::list_cards(
        &database,
        &repo,
        kanban::Direction::Inbound,
        kanban::CardScope::WorkingSet,
    )?;
    // Prints nothing when no card is Accepted -- the goal is "cleared"
    // by board state alone (AC: clears on terminal/blocked).
    if let Some(goal) = kanban::format_active_goal(&cards) {
        println!("{goal}");
    }
    Ok(())
}

pub(crate) fn handle_statusline(json: bool) -> error::Result<()> {
    // Never surface an error to the Claude Code UI: statusline::run
    // already swallows internal failures and logs them, and its
    // signature reflects that contract (cannot return an error).
    statusline::run(json);
    Ok(())
}
