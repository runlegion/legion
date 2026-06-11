mod autonomy;
mod board;
mod card_parse;
mod channel;
mod cluster;
mod daemon;
mod db;
mod documents;
mod embed;
mod error;
mod health;
mod init;
#[allow(dead_code)] // Items used by tests + pending surface/status/serve migration
mod kanban;
mod mcp;
mod mesh;
mod now;
mod pr_view;
mod pr_write;
mod pty;
mod recall;
mod reflect;
mod scip;
mod search;
mod serve;
mod signal;
mod stats;
mod status;
mod statusline;
mod surface;
mod sym;
mod sync;
mod sync_actor;
mod task;
mod telemetry;
#[cfg(test)]
mod testutil;
mod timefmt;
mod uncertainty;
mod usage;
mod verbs;
mod verify;
mod wake_attempts;
mod watch;
mod worksource;

use std::sync::atomic::{AtomicBool, Ordering};

use clap::Parser;

use cli::util::raise_fd_limit;
use cli::{Cli, Commands};

static VERBOSE: AtomicBool = AtomicBool::new(false);

/// Print an informational message to stderr, only when --verbose is set.
macro_rules! info {
    ($($arg:tt)*) => {
        if crate::VERBOSE.load(std::sync::atomic::Ordering::Relaxed) {
            eprintln!($($arg)*);
        }
    };
}

// Declared after the `info!` macro so the textual macro scope reaches the
// handler modules (macro_rules scope descends into modules declared after
// the definition).
mod cli;

// Re-export hub: external modules keep using `crate::data_dir` and
// `crate::ClusterAction` (cluster.rs) unchanged.
pub(crate) use cli::datadir::data_dir;
pub(crate) use cli::ops::ClusterAction;

fn main() -> error::Result<()> {
    // Windows default stack (1MB) is too small for clap + Tantivy init.
    // Spawn with 8MB stack to match macOS/Linux defaults.
    const STACK_SIZE: usize = 8 * 1024 * 1024;
    let builder = std::thread::Builder::new().stack_size(STACK_SIZE);
    let handler = builder.spawn(run).map_err(error::LegionError::Io)?;
    handler.join().unwrap_or_else(|_| {
        Err(error::LegionError::Io(std::io::Error::other(
            "thread panicked",
        )))
    })
}

fn run() -> error::Result<()> {
    raise_fd_limit();
    let cli = Cli::parse();
    VERBOSE.store(cli.verbose, Ordering::Relaxed);

    match cli.command {
        Commands::Reflect {
            repo,
            text,
            transcript,
            domain,
            whoami,
            tags,
            follows,
            force,
            dedupe_mode,
        } => cli::memory::handle_reflect(
            repo,
            text,
            transcript,
            domain,
            whoami,
            tags,
            follows,
            force,
            dedupe_mode,
        )?,
        Commands::Forget { id, repo } => cli::memory::handle_forget(id, repo)?,
        Commands::Recall {
            repo,
            context,
            limit,
            latest,
            preview,
            cosine_only,
            min_score,
            domain,
            archives,
            include_archives,
        } => cli::memory::handle_recall(
            repo,
            context,
            limit,
            latest,
            preview,
            cosine_only,
            min_score,
            domain,
            archives,
            include_archives,
        )?,
        Commands::Similar {
            id,
            limit,
            cross_repo,
            min_score,
            preview,
            json,
        } => cli::memory::handle_similar(id, limit, cross_repo, min_score, preview, json)?,
        Commands::Consult {
            context,
            symbol,
            limit,
            json,
        } => cli::memory::handle_consult(context, symbol, limit, json)?,
        Commands::Post {
            repo,
            text,
            transcript,
            domain,
            tags,
            follows,
        } => cli::signal::handle_post(repo, text, transcript, domain, tags, follows)?,
        Commands::Signal {
            repo,
            to,
            verb,
            status,
            note,
            details,
            follows,
            domain,
            tags,
        } => cli::signal::handle_signal(
            repo, to, verb, status, note, details, follows, domain, tags,
        )?,
        Commands::PendingReplies { repo } => cli::signal::handle_pending_replies(repo)?,
        Commands::Boost { id } => cli::memory::handle_boost(id)?,
        Commands::Resolve { id, reflection } => cli::memory::handle_resolve(id, reflection)?,
        Commands::Chain { id, full } => cli::memory::handle_chain(id, full)?,
        Commands::Bullpen {
            repo,
            count,
            signals,
            musings,
            archive,
            archived,
            include_stale,
            include_resolved,
        } => cli::signal::handle_bullpen(
            repo,
            count,
            signals,
            musings,
            archive,
            archived,
            include_stale,
            include_resolved,
        )?,
        Commands::Index {
            repo,
            file,
            status,
            logs,
            follow,
            lines,
            banner,
            json,
        } => cli::index_cmd::handle_index(repo, file, status, logs, follow, lines, banner, json)?,
        Commands::Sym { action } => cli::index_cmd::handle_sym(action)?,
        Commands::Reindex => cli::index_cmd::handle_reindex()?,
        Commands::Cleanup { retention_days } => cli::index_cmd::handle_cleanup(retention_days)?,
        Commands::Rename { from, to } => cli::index_cmd::handle_rename(from, to)?,
        Commands::Backfill => cli::memory::handle_backfill()?,
        Commands::Document { action } => cli::document::handle(action)?,
        Commands::SubIssue { action } => cli::issue::handle_sub_issue(action)?,
        Commands::McpHealth { wait } => cli::misc::handle_mcp_health(wait)?,
        Commands::Now { banner: _, json } => cli::misc::handle_now(json)?,
        Commands::Init { force } => cli::misc::handle_init(force)?,
        Commands::Surface { repo } => cli::misc::handle_surface(repo)?,
        Commands::Whoami { repo, limit } => cli::misc::handle_whoami(repo, limit)?,
        Commands::Whatami { repo, limit } => cli::misc::handle_whatami(repo, limit)?,
        Commands::Stats { repo } => cli::misc::handle_stats(repo)?,
        Commands::Telemetry { action } => cli::ops::handle_telemetry(action)?,
        Commands::Uncertainty { action } => cli::ops::handle_uncertainty(action)?,
        Commands::Serve { port } => cli::misc::handle_serve(port)?,
        Commands::Status { repo, json } => cli::misc::handle_status(repo, json)?,
        Commands::Needs { repo } => cli::misc::handle_needs(repo)?,
        Commands::Done { repo, text, id } => cli::kanban::handle_done(repo, text, id)?,
        Commands::Work { repo, peek } => cli::misc::handle_work(repo, peek)?,
        Commands::Sync { repo } => cli::misc::handle_sync(repo)?,
        Commands::Cluster { action } => cli::ops::handle_cluster(action)?,
        Commands::Kanban { action } => cli::kanban::handle(action)?,
        Commands::Task { action } => cli::misc::handle_task(action)?,
        Commands::Schedule { action } => cli::schedule::handle(action)?,
        Commands::Issue { action } => cli::issue::handle(action)?,
        Commands::Pr { action } => cli::pr::handle(action)?,
        Commands::Comment { repo, number, body } => cli::issue::handle_comment(repo, number, body)?,
        Commands::Audit {
            repo,
            action,
            limit,
            json,
        } => cli::ops::handle_audit(repo, action, limit, json)?,
        Commands::Watch { action } => cli::watch::handle(action)?,
        Commands::Mcp => cli::misc::handle_mcp()?,
        Commands::McpLogs { pid, tail } => cli::misc::handle_mcp_logs(pid, tail)?,
        Commands::Daemon { port } => cli::misc::handle_daemon(port)?,
        Commands::DaemonSpawn { port } => cli::misc::handle_daemon_spawn(port)?,
        Commands::DaemonStop => cli::misc::handle_daemon_stop()?,
        Commands::DaemonRestart { port } => cli::misc::handle_daemon_restart(port)?,
        Commands::QualityGate { action } => cli::verify::handle_quality_gate(action)?,
        Commands::Verify {
            repo: _repo,
            card,
            verdicts_file,
        } => cli::verify::handle_verify(card, verdicts_file)?,
        Commands::Autonomy { action } => cli::autonomy::handle(action)?,
        Commands::Goal { repo } => cli::misc::handle_goal(repo)?,
        Commands::Statusline { json } => cli::misc::handle_statusline(json)?,
        Commands::Mesh { action } => cli::ops::handle_mesh(action)?,
        Commands::Usage {
            session,
            since,
            today: _today,
            by_session,
            by_repo,
            json,
        } => cli::ops::handle_usage(session, since, by_session, by_repo, json)?,
        Commands::Health {
            history,
            all_hosts,
            json,
        } => cli::ops::handle_health(history, all_hosts, json)?,
    }

    Ok(())
}
