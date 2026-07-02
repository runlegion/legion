//! `legion index`/`sym`/`reindex`/`cleanup`/`rename` handlers and the
//! background indexer plumbing (carved from main.rs, #610).

use std::path::PathBuf;

use clap::Subcommand;

use crate::cli::datadir::data_dir;
use crate::cli::util::{open_db, open_db_and_index};
use crate::{db, error, inventory, scip, sym, watch};

#[derive(Subcommand)]
pub(crate) enum SymAction {
    /// Print definitions of a symbol
    Def {
        /// Symbol name to look up (substring-matched against SCIP symbol strings)
        name: String,
        /// Restrict to a single repo (default: every repo with a stored index)
        #[arg(long)]
        repo: Option<String>,
        /// Restrict to a single language (default: every language with a stored index)
        #[arg(long)]
        lang: Option<String>,
        /// Emit results as a JSON array of SymbolLocation objects
        #[arg(long)]
        json: bool,
    },

    /// Print references / call sites of a symbol
    Refs {
        /// Symbol name to look up (substring-matched against SCIP symbol strings)
        name: String,
        #[arg(long)]
        repo: Option<String>,
        #[arg(long)]
        lang: Option<String>,
        #[arg(long)]
        json: bool,
    },

    /// Print types implementing a trait or interface
    Impl {
        /// Trait or interface name (substring-matched against SCIP symbol strings)
        trait_name: String,
        #[arg(long)]
        repo: Option<String>,
        #[arg(long)]
        lang: Option<String>,
        #[arg(long)]
        json: bool,
    },

    /// Print signature + docstring for a symbol
    Hover {
        /// Symbol name to look up (substring-matched against SCIP symbol strings)
        name: String,
        #[arg(long)]
        repo: Option<String>,
        #[arg(long)]
        lang: Option<String>,
        #[arg(long)]
        json: bool,
    },

    /// Impact radius: for every symbol whose definition the given diff
    /// touches, report the SCIP reference count across the repo's index.
    /// Used by smugglr to flag wide-blast-radius PRs at review time.
    Impact {
        /// Repo whose SCIP index to query
        #[arg(long)]
        repo: String,
        /// Path to a unified diff file, or "-" to read from stdin
        #[arg(long)]
        diff: String,
        /// Emit results as JSON instead of human-readable text
        #[arg(long)]
        json: bool,
    },

    /// Enumerate the definitions in the index -- the "what functions/enums/etc.
    /// are defined here" query. Byte-cheap: names + locations, never source
    /// bodies. This is the shape `grep "fn "` was serving; use it instead.
    List {
        /// Restrict to a single repo (default: every repo with a stored index)
        #[arg(long)]
        repo: Option<String>,
        /// Restrict to a single language
        #[arg(long)]
        lang: Option<String>,
        /// Filter by kind: fn, struct, enum, trait, class, interface, mod,
        /// const, macro, type ("type" matches all type-like definitions).
        #[arg(long)]
        kind: Option<String>,
        /// Scope to one file (exact relative path or a path suffix)
        #[arg(long)]
        file: Option<String>,
        /// Emit results as a JSON array of SymbolEntry objects
        #[arg(long)]
        json: bool,
    },
}

/// Dispatch `legion sym <action>` against the local index store.
///
/// Loads every matching `scip_indexes` row (filtered by optional repo and
/// language), runs the per-blob query, and prints results to stdout.
/// Exits with code 1 and a stderr message when no index rows match the
/// filter, distinguishing "no data" from "empty result."
fn run_sym_action(database: &db::Database, action: SymAction) -> error::Result<()> {
    match action {
        SymAction::Def {
            name,
            repo,
            lang,
            json,
        } => run_location_query(database, repo, lang, json, false, |idx| {
            sym::query_definitions(&idx.blob, &name, &idx.repo, &idx.lang)
        }),
        SymAction::Refs {
            name,
            repo,
            lang,
            json,
        } => run_location_query(database, repo, lang, json, false, |idx| {
            sym::query_references(&idx.blob, &name, &idx.repo, &idx.lang)
        }),
        SymAction::Impl {
            trait_name,
            repo,
            lang,
            json,
        } => run_location_query(database, repo, lang, json, true, |idx| {
            sym::query_implementors(&idx.blob, &trait_name, &idx.repo, &idx.lang)
        }),
        SymAction::Hover {
            name,
            repo,
            lang,
            json,
        } => run_hover_query(database, &name, repo, lang, json),
        SymAction::Impact { repo, diff, json } => run_sym_impact(database, &repo, &diff, json),
        SymAction::List {
            repo,
            lang,
            kind,
            file,
            json,
        } => run_sym_list(database, repo, lang, kind, file, json),
    }
}

/// Enumerate definitions across the matching SCIP indexes (#558). Validates
/// the `--kind` filter up front (unknown kind = loud error, not silent empty),
/// runs `sym::query_symbols` per blob, and prints byte-cheap entries. A repo
/// with no index, or no matching definitions, is a clean exit 0 (enumeration
/// of nothing is informational, not a lookup failure).
fn run_sym_list(
    database: &db::Database,
    repo: Option<String>,
    lang: Option<String>,
    kind: Option<String>,
    file: Option<String>,
    json: bool,
) -> error::Result<()> {
    use std::io::Write;

    let norm_kind = match kind.as_deref() {
        Some(k) => match sym::normalize_kind_filter(k) {
            Some(n) => Some(n),
            None => {
                eprintln!(
                    "[legion] unknown --kind '{k}'. Supported: fn, struct, enum, trait, \
                     class, interface, mod, const, macro, type"
                );
                return Err(error::LegionError::ExitWith(2));
            }
        },
        None => None,
    };

    let indexes = database.list_scip_indexes_filtered(repo.as_deref(), lang.as_deref())?;
    if indexes.is_empty() {
        no_index_found(repo.as_deref(), lang.as_deref());
        return Ok(());
    }

    let mut all = Vec::new();
    for idx in &indexes {
        let mut hits =
            sym::query_symbols(&idx.blob, norm_kind, file.as_deref(), &idx.repo, &idx.lang)?;
        all.append(&mut hits);
    }
    all.sort_by(|a, b| {
        a.repo
            .cmp(&b.repo)
            .then(a.lang.cmp(&b.lang))
            .then(a.file.cmp(&b.file))
            .then(a.line.cmp(&b.line))
            .then(a.name.cmp(&b.name))
    });

    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    if json {
        serde_json::to_writer(&mut out, &all)?;
        writeln!(out)?;
    } else {
        for e in &all {
            writeln!(
                out,
                "{}\t{}:{}\t[{}] {}/{}",
                e.name, e.file, e.line, e.kind, e.repo, e.lang
            )?;
        }
        if all.is_empty() {
            eprintln!("[legion] no matching definitions");
        }
    }
    Ok(())
}

/// Read the diff source -- file path or "-" for stdin -- and run impact
/// radius analysis against the repo's SCIP index. Prints sorted output
/// (highest refs_count first) as either text or JSON.
fn run_sym_impact(
    database: &db::Database,
    repo: &str,
    diff_arg: &str,
    json: bool,
) -> error::Result<()> {
    use std::io::{Read, Write};

    let diff_text = if diff_arg == "-" {
        let mut buf = String::new();
        std::io::stdin()
            .read_to_string(&mut buf)
            .map_err(error::LegionError::Io)?;
        buf
    } else {
        std::fs::read_to_string(diff_arg).map_err(error::LegionError::Io)?
    };

    let indexes = database.list_scip_indexes_filtered(Some(repo), None)?;
    if indexes.is_empty() {
        eprintln!("[legion] no SCIP index for repo '{repo}' -- run `legion index {repo}` first");
        return Ok(());
    }

    // Cross-index dedup: a polyglot repo can have the same logical symbol
    // appear in two language indexes. `diff_impact_radius` dedupes within
    // one blob; this loop dedupes across blobs so the CLI never prints
    // the same symbol twice.
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut all_hits: Vec<sym::ImpactRadius> = Vec::new();
    for idx in &indexes {
        let hits = sym::diff_impact_radius(&idx.blob, &diff_text)?;
        for hit in hits {
            if seen.insert(hit.symbol.clone()) {
                all_hits.push(hit);
            }
        }
    }
    all_hits.sort_by_key(|h| std::cmp::Reverse(h.refs_count));

    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    if json {
        let body = serde_json::to_string(&all_hits)
            .map_err(|e| error::LegionError::Search(format!("impact json: {e}")))?;
        writeln!(out, "{body}")?;
        return Ok(());
    }
    if all_hits.is_empty() {
        writeln!(out, "[legion] no symbol definitions touched by this diff")?;
        return Ok(());
    }
    let high_threshold: u32 = std::env::var("LEGION_IMPACT_HIGH_THRESHOLD")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(50);
    for hit in &all_hits {
        let high = if hit.refs_count >= high_threshold {
            "  HIGH"
        } else {
            ""
        };
        let loc = match &hit.def_location {
            Some(loc) => format!("{}:{}", loc.file, loc.line),
            None => "(no def location)".to_string(),
        };
        writeln!(out, "{loc}\t{}\trefs:{}{high}", hit.symbol, hit.refs_count)?;
    }
    Ok(())
}

/// One row in the cross-repo symbol consult output.
#[derive(serde::Serialize)]
struct ConsultSymbolHit {
    repo: String,
    lang: String,
    file: String,
    line: u32,
    column: u32,
    refs_count: usize,
}

/// Implementation of `legion consult --symbol <name>` (#285).
///
/// Walks every (repo, lang) pair in `scip_indexes`, finds matching
/// definitions, counts references per match. Output is sorted by repo
/// then lang then file. Empty result exits 0 silently in human mode and
/// emits `[]` in JSON mode -- a thin response is data, not failure.
pub(crate) fn run_consult_symbol(
    database: &db::Database,
    name: &str,
    json: bool,
) -> error::Result<()> {
    use std::io::Write;
    let indexes = database.list_scip_indexes_filtered(None, None)?;

    let mut hits: Vec<ConsultSymbolHit> = Vec::new();
    for idx in &indexes {
        let defs = match sym::query_definitions(&idx.blob, name, &idx.repo, &idx.lang) {
            Ok(d) => d,
            Err(e) => {
                eprintln!("[legion] skipped {}/{}: {e}", idx.repo, idx.lang);
                continue;
            }
        };
        if defs.is_empty() {
            continue;
        }
        let refs_count = sym::query_references(&idx.blob, name, &idx.repo, &idx.lang)
            .map(|r| r.len())
            .unwrap_or(0);
        for d in defs {
            hits.push(ConsultSymbolHit {
                repo: d.repo,
                lang: d.lang,
                file: d.file,
                line: d.line,
                column: d.column,
                refs_count,
            });
        }
    }

    hits.sort_by(|a, b| {
        a.repo
            .cmp(&b.repo)
            .then(a.lang.cmp(&b.lang))
            .then(a.file.cmp(&b.file))
            .then(a.line.cmp(&b.line))
    });

    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    if json {
        serde_json::to_writer(&mut out, &hits)?;
        writeln!(out)?;
    } else if hits.is_empty() {
        info!("[legion] no SCIP index has a definition for `{}`", name);
    } else {
        for h in &hits {
            writeln!(
                out,
                "{}/{}\t{}:{}:{}\t({} ref(s))",
                h.repo, h.lang, h.file, h.line, h.column, h.refs_count
            )?;
        }
    }
    Ok(())
}

/// Spawn `legion index <repo>` as a detached background process (#284).
///
/// Called from `WatchAction::Add` so a freshly-watched repo gets its
/// SCIP indexes populated without blocking the operator. Both stdout
/// and stderr go to a per-repo log file under the temp dir; on any
/// failure the watch add still succeeds and a warning is printed --
/// background indexing is best-effort, the operator can always run
/// `legion index <repo>` manually later.
pub(crate) fn spawn_background_indexer(repo_name: &str) {
    let self_path = match std::env::current_exe() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("[legion] background indexer not started: cannot resolve own binary: {e}");
            return;
        }
    };
    // One-shot migration: on every spawn, sweep any leftover
    // /tmp/legion-index-*.log files from the prior /tmp-rooted location into
    // the new XDG_STATE_HOME directory. Idempotent so the next spawn is a
    // no-op once the migration ran.
    scip::migrate_legacy_index_logs();
    let log_path = scip::index_log_path(repo_name);
    if let Some(parent) = log_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let log_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path);
    let (stdout, stderr, log_note) = match log_file {
        Ok(f) => match f.try_clone() {
            Ok(f2) => (
                std::process::Stdio::from(f),
                std::process::Stdio::from(f2),
                format!("indexing in background; log: {}", log_path.display()),
            ),
            Err(_) => (
                std::process::Stdio::from(f),
                std::process::Stdio::null(),
                format!(
                    "indexing in background; stdout log: {} (stderr discarded)",
                    log_path.display()
                ),
            ),
        },
        Err(e) => {
            eprintln!(
                "[legion] background indexer log {} not writable: {e} -- spawning silently",
                log_path.display()
            );
            (
                std::process::Stdio::null(),
                std::process::Stdio::null(),
                "indexing in background (no log)".to_string(),
            )
        }
    };
    match std::process::Command::new(self_path)
        .arg("index")
        .arg(repo_name)
        .stdin(std::process::Stdio::null())
        .stdout(stdout)
        .stderr(stderr)
        .spawn()
    {
        Ok(_) => println!("{log_note}"),
        Err(e) => eprintln!("[legion] background indexer spawn failed: {e}"),
    }
}

/// Print the tail of every per-repo background-indexer log, optionally
/// filtered to one repo. With `follow = true`, tails new output as it
/// arrives until SIGINT.
fn run_index_logs(repo: Option<&str>, tail_lines: usize, follow: bool) -> error::Result<()> {
    use std::io::Write;
    let stdout = std::io::stdout();
    let mut out = stdout.lock();

    let initial = scip::read_index_logs(repo, tail_lines)?;
    if initial.is_empty() {
        let dir = scip::index_log_dir();
        writeln!(
            out,
            "[legion] no index logs in {} -- run `legion watch add <repo>` or `legion index <repo>` to generate one",
            dir.display()
        )?;
        return Ok(());
    }
    for (label, content) in &initial {
        writeln!(out, "=== {label} ===")?;
        writeln!(out, "{content}")?;
        writeln!(out)?;
    }
    out.flush()?;
    if !follow {
        return Ok(());
    }

    // Track per-repo byte offsets so we only print new content on each
    // poll. Initialized at current end-of-file so the first follow cycle
    // emits only fresh writes (we already printed the initial tail above).
    use std::collections::HashMap;
    let mut offsets: HashMap<String, u64> = HashMap::new();
    let dir = scip::index_log_dir();
    if dir.is_dir()
        && let Ok(entries) = std::fs::read_dir(&dir)
    {
        for entry in entries.flatten() {
            let path = entry.path();
            let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };
            if let Some(filter) = repo
                && stem != filter
            {
                continue;
            }
            if let Ok(meta) = std::fs::metadata(&path) {
                offsets.insert(stem.to_string(), meta.len());
            }
        }
    }

    // SIGINT (Ctrl-C) terminates the process via the OS default handler;
    // there is no per-iteration cleanup state to preserve, so the polling
    // loop has no explicit signal handling.
    loop {
        std::thread::sleep(std::time::Duration::from_millis(250));
        if !dir.is_dir() {
            continue;
        }
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        let mut paths: Vec<std::path::PathBuf> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.is_file() && p.extension().and_then(|e| e.to_str()) == Some("log"))
            .collect();
        paths.sort();
        for path in paths {
            let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };
            if let Some(filter) = repo
                && stem != filter
            {
                continue;
            }
            let Ok(meta) = std::fs::metadata(&path) else {
                continue;
            };
            let last = offsets.get(stem).copied().unwrap_or(0);
            let now = meta.len();
            if now <= last {
                continue;
            }
            // Read only the new bytes by seeking past `last`.
            use std::io::{Read, Seek, SeekFrom};
            let Ok(mut f) = std::fs::File::open(&path) else {
                continue;
            };
            if f.seek(SeekFrom::Start(last)).is_err() {
                continue;
            }
            let mut buf = String::new();
            if f.read_to_string(&mut buf).is_err() {
                continue;
            }
            if buf.is_empty() {
                continue;
            }
            writeln!(out, "=== {stem} ===")?;
            write!(out, "{buf}")?;
            out.flush()?;
            offsets.insert(stem.to_string(), now);
        }
    }
}

/// Render a one-shot SCIP index health banner for a single repo, intended
/// to be appended to the SessionStart hook output. Silent on healthy
/// (every detected language has a fresh index); loud on stale, missing,
/// or unindexable repos. Never errors -- a banner-mode failure prints
/// "unavailable" and returns Ok so SessionStart never blocks on this.
fn run_index_status_banner(base: &std::path::Path, repo: Option<&str>) -> error::Result<()> {
    use std::io::Write;
    let stdout = std::io::stdout();
    let mut out = stdout.lock();

    let Some(repo_name) = repo else {
        // --banner without a repo is a misconfigured caller. Print a quiet
        // hint so the failure is observable but exit cleanly.
        writeln!(
            out,
            "[Legion] Index status: --banner requires <repo> -- pass the repo name positionally"
        )?;
        return Ok(());
    };

    let watch_path = base.join("watch.toml");
    let repos = match watch::list_repos_in_config(&watch_path) {
        Ok(r) => r,
        Err(_) => {
            writeln!(out, "[Legion] Index status: unavailable")?;
            return Ok(());
        }
    };
    let entry = match repos.iter().find(|r| r.name == repo_name) {
        Some(e) => e,
        None => {
            // Repo is not watched. Quiet for SessionStart -- this is the
            // common case for one-off shells in unrelated dirs.
            return Ok(());
        }
    };
    let repo_path = std::path::PathBuf::from(&entry.workdir);
    let detected = scip::detect_languages(&repo_path);

    let database = match db::Database::open(&base.join("legion.db")) {
        Ok(d) => d,
        Err(_) => {
            writeln!(out, "[Legion] Index status: unavailable")?;
            return Ok(());
        }
    };
    let indexes = match database.list_scip_indexes_filtered(Some(repo_name), None) {
        Ok(i) => i,
        Err(_) => {
            writeln!(out, "[Legion] Index status: unavailable")?;
            return Ok(());
        }
    };

    let now = chrono::Utc::now();
    let banner = render_index_status_banner(repo_name, &detected, &indexes, now);
    if !banner.is_empty() {
        writeln!(out, "{banner}")?;
    }
    Ok(())
}

/// Pure renderer for the SessionStart index status banner. Empty string
/// means "silent" (every detected language has a fresh index). Returns a
/// multi-line block when anything is stale or missing.
///
/// Stale threshold: 7 days. Override-friendly through a build constant if
/// future tuning needs it; not exposed as a CLI flag in v1.
fn render_index_status_banner(
    repo_name: &str,
    detected_langs: &[&str],
    indexes: &[scip::ScipIndex],
    now: chrono::DateTime<chrono::Utc>,
) -> String {
    const STALE_THRESHOLD_DAYS: i64 = 7;

    if detected_langs.is_empty() {
        // No supported language detected -- silent. Avoids cluttering
        // SessionStart for repos that legion does not index.
        return String::new();
    }

    enum LangHealth {
        Fresh { lang: String, age: String },
        Stale { lang: String, age: String },
        Missing { lang: String },
    }

    fn humanize_age(seconds: i64) -> String {
        if seconds < 60 {
            return "just now".to_string();
        }
        if seconds < 3600 {
            return format!("{}m ago", seconds / 60);
        }
        if seconds < 86_400 {
            return format!("{}h ago", seconds / 3600);
        }
        format!("{}d ago", seconds / 86_400)
    }

    let mut report = Vec::with_capacity(detected_langs.len());
    for lang in detected_langs {
        let row = indexes.iter().find(|i| i.lang == *lang);
        match row {
            Some(idx) => match chrono::DateTime::parse_from_rfc3339(&idx.updated_at) {
                Ok(parsed) => {
                    let age_seconds = (now - parsed.with_timezone(&chrono::Utc)).num_seconds();
                    let age = humanize_age(age_seconds);
                    if age_seconds > STALE_THRESHOLD_DAYS * 86_400 {
                        report.push(LangHealth::Stale {
                            lang: (*lang).to_string(),
                            age,
                        });
                    } else {
                        report.push(LangHealth::Fresh {
                            lang: (*lang).to_string(),
                            age,
                        });
                    }
                }
                Err(_) => {
                    // Unparseable timestamp -- treat as stale so the operator
                    // sees a signal to rebuild rather than silently trusting
                    // an index whose freshness we cannot verify.
                    report.push(LangHealth::Stale {
                        lang: (*lang).to_string(),
                        age: "unknown age".to_string(),
                    });
                }
            },
            None => report.push(LangHealth::Missing {
                lang: (*lang).to_string(),
            }),
        }
    }

    let all_fresh = report.iter().all(|h| matches!(h, LangHealth::Fresh { .. }));
    if all_fresh {
        let summary: Vec<String> = report
            .iter()
            .map(|h| match h {
                LangHealth::Fresh { lang, age } => format!("{lang}: fresh ({age})"),
                _ => unreachable!(),
            })
            .collect();
        return format!(
            "[Legion] Index status for {repo_name}: {}",
            summary.join(", ")
        );
    }

    let mut lines = vec![format!("[Legion] Index status for {repo_name}:")];
    for h in &report {
        match h {
            LangHealth::Fresh { lang, age } => {
                lines.push(format!("  {lang}: fresh ({age})"));
            }
            LangHealth::Stale { lang, age } => {
                lines.push(format!(
                    "  {lang}: STALE -- last built {age}, run `legion index {repo_name}` to refresh"
                ));
            }
            LangHealth::Missing { lang } => {
                lines.push(format!(
                    "  {lang}: not indexed -- run `legion index {repo_name}` or `legion watch add {repo_name}` to build"
                ));
            }
        }
    }
    lines.join("\n")
}

fn run_location_query<F>(
    database: &db::Database,
    repo: Option<String>,
    lang: Option<String>,
    json: bool,
    rust_only: bool,
    mut query: F,
) -> error::Result<()>
where
    F: FnMut(&scip::ScipIndex) -> error::Result<Vec<sym::SymbolLocation>>,
{
    use std::io::Write;
    let indexes = database.list_scip_indexes_filtered(repo.as_deref(), lang.as_deref())?;
    if indexes.is_empty() {
        no_index_found(repo.as_deref(), lang.as_deref());
        return Err(error::LegionError::ExitWith(1));
    }

    // Skip non-rust indexes for `impl` queries (SCIP only models the
    // relationship for languages with traits/interfaces). Stay quiet
    // when `--lang` already restricts the scope -- the user clearly
    // knows the constraint.
    let lang_filter_active = lang.is_some();
    let mut all = Vec::new();
    for idx in &indexes {
        if rust_only && idx.lang != "rust" {
            if !lang_filter_active {
                eprintln!(
                    "[legion] note: 'impl' relationships not modeled for {} -- skipping {}/{}",
                    idx.lang, idx.repo, idx.lang
                );
            }
            continue;
        }
        let mut hits = query(idx)?;
        all.append(&mut hits);
    }
    all.sort_by(|a, b| {
        a.repo
            .cmp(&b.repo)
            .then(a.lang.cmp(&b.lang))
            .then(a.file.cmp(&b.file))
            .then(a.line.cmp(&b.line))
    });

    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    if json {
        serde_json::to_writer(&mut out, &all)?;
        writeln!(out)?;
    } else {
        for loc in &all {
            writeln!(
                out,
                "{}:{}:{}\t[{}/{}]",
                loc.file, loc.line, loc.column, loc.repo, loc.lang
            )?;
        }
    }
    Ok(())
}

/// First-match hover lookup. Iterates indexes in the order returned by
/// `list_scip_indexes_filtered` (sorted by repo, lang) and returns the
/// first symbol that matches. When the query spans multiple indexes,
/// callers typically scope with `--repo` / `--lang` to disambiguate.
fn run_hover_query(
    database: &db::Database,
    name: &str,
    repo: Option<String>,
    lang: Option<String>,
    json: bool,
) -> error::Result<()> {
    use std::io::Write;
    let indexes = database.list_scip_indexes_filtered(repo.as_deref(), lang.as_deref())?;
    if indexes.is_empty() {
        no_index_found(repo.as_deref(), lang.as_deref());
        return Err(error::LegionError::ExitWith(1));
    }
    let mut hover: Option<sym::HoverInfo> = None;
    for idx in &indexes {
        if let Some(h) = sym::query_hover(&idx.blob, name, &idx.repo, &idx.lang)? {
            hover = Some(h);
            break;
        }
    }

    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    if json {
        serde_json::to_writer(&mut out, &hover)?;
        writeln!(out)?;
    } else if let Some(h) = hover {
        writeln!(out, "{}", h.symbol)?;
        if let Some(sig) = h.signature {
            writeln!(out, "{sig}")?;
        }
        if let Some(doc) = h.docstring {
            writeln!(out)?;
            writeln!(out, "{doc}")?;
        }
    }
    Ok(())
}

fn no_index_found(repo: Option<&str>, lang: Option<&str>) {
    let scope = match (repo, lang) {
        (Some(r), Some(l)) => format!("{r}/{l}"),
        (Some(r), None) => r.to_string(),
        (None, Some(l)) => format!("(any repo)/{l}"),
        (None, None) => "(any repo)".to_string(),
    };
    eprintln!("[legion] no index found for {scope}; run `legion index <repo>` first");
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn handle_index(
    repo: Option<String>,
    file: Option<PathBuf>,
    status: bool,
    logs: bool,
    follow: bool,
    lines: usize,
    banner: bool,
    json: bool,
) -> error::Result<()> {
    let base = data_dir()?;

    // --logs: print recent background-indexer log content and return.
    if logs {
        run_index_logs(repo.as_deref(), lines, follow)?;
        return Ok(());
    }

    // --status --banner: SessionStart-friendly per-repo health line.
    if status && banner {
        run_index_status_banner(&base, repo.as_deref())?;
        return Ok(());
    }

    // --status: dump scip_indexes inventory and return.
    if status {
        let database = open_db()?;
        let indexes = database.list_scip_indexes_filtered(repo.as_deref(), None)?;
        if json {
            let rows: Vec<serde_json::Value> = indexes
                .iter()
                .map(|idx| {
                    serde_json::json!({
                        "repo": idx.repo,
                        "lang": idx.lang,
                        "size_bytes": idx.blob.len(),
                        "updated_at": idx.updated_at,
                    })
                })
                .collect();
            println!("{}", serde_json::to_string(&rows)?);
        } else if indexes.is_empty() {
            info!("[legion] no SCIP indexes recorded yet");
        } else {
            use std::io::Write;
            let stdout = std::io::stdout();
            let mut out = stdout.lock();
            for idx in &indexes {
                let bytes = idx.blob.len();
                writeln!(
                    out,
                    "{}/{}\t{} bytes\t{}",
                    idx.repo, idx.lang, bytes, idx.updated_at
                )?;
            }
        }
        return Ok(());
    }

    let watch_path = base.join("watch.toml");
    let repos = watch::list_repos_in_config(&watch_path)?;

    // --file overrides --repo: resolve the owning repo by walking
    // ancestors of the file path against watch.toml workdirs.
    // Falls through to the named-repo path with a synthesized repo
    // name so the indexer + upsert loop is the single code path.
    let (repo, entry): (String, &watch::WatchRepoConfig) = match (repo, file.as_ref()) {
        (_, Some(file_path)) => {
            let canon = std::fs::canonicalize(file_path).map_err(|e| {
                error::LegionError::WatchConfig(format!(
                    "cannot canonicalize {}: {e}",
                    file_path.display()
                ))
            })?;
            let owner = repos
                .iter()
                .find(|r| {
                    std::fs::canonicalize(&r.workdir)
                        .map(|w| canon.starts_with(&w))
                        .unwrap_or(false)
                })
                .ok_or_else(|| {
                    error::LegionError::WatchConfig(format!(
                        "no watch.toml entry owns {} -- file is outside every watched workdir",
                        canon.display()
                    ))
                })?;
            (owner.name.clone(), owner)
        }
        (Some(name), None) => {
            let owner = repos.iter().find(|r| r.name == name).ok_or_else(|| {
                error::LegionError::WatchConfig(format!(
                    "repo '{name}' not in watch.toml. Add it with `legion watch add {name} <path>`."
                ))
            })?;
            (name, owner)
        }
        (None, None) => {
            return Err(error::LegionError::WatchConfig(
                "either <repo> or --file <path> is required".to_string(),
            ));
        }
    };
    let repo_path = std::path::PathBuf::from(&entry.workdir);

    // A missing workdir must fail BEFORE the walk: walk_repo on a vanished
    // root returns an empty entry set, and pruning against it would delete
    // every inventory row for the repo -- a transient mount failure (watched
    // workdirs live on external volumes) silently destroying derived state
    // (#718 review).
    if !repo_path.is_dir() {
        return Err(error::LegionError::WatchConfig(format!(
            "workdir {} for repo '{repo}' does not exist or is not a directory -- \
             refusing to index (an unmounted volume must not wipe the inventory)",
            repo_path.display()
        )));
    }

    let langs = scip::detect_languages(&repo_path);
    let database = open_db()?;

    // File inventory walk FIRST: enumerate every non-ignored file and persist
    // one row per file. Ordered before the SCIP loop so inventory truly runs
    // independent of SCIP outcomes -- including the all-indexers-failed hard
    // error below, which must not leave the inventory stale (#705 review).
    let outcome = inventory::walk_repo(&repo, &repo_path);
    let live_paths: Vec<String> = outcome.entries.iter().map(|e| e.path.clone()).collect();
    database.upsert_file_inventory(&outcome.entries)?;
    // Prune only on a complete walk: with walk errors the entry set may be
    // missing files that still exist, and evicting their rows would let a
    // transient I/O failure shrink the inventory (#718 re-review). Upserting
    // the partial set is still correct -- fresh data is fresh.
    let pruned: usize = if outcome.walk_errors == 0 {
        database.prune_file_inventory(&repo, &live_paths)?
    } else {
        eprintln!(
            "[legion] {} walk errors for {repo} -- stale-row prune skipped (partial walk must not evict rows)",
            outcome.walk_errors
        );
        0
    };
    eprintln!(
        "[legion] inventoried {} files for {repo} ({pruned} stale rows pruned)",
        outcome.entries.len()
    );

    // SCIP indexing runs only when language markers are present. A docs-only
    // repo (no Cargo.toml, package.json, etc.) skips this block entirely --
    // the file inventory above already covered it (#705).
    if langs.is_empty() {
        eprintln!(
            "[legion] no SCIP-supported language detected at {}; skipping SCIP indexing. \
             Markers checked: Cargo.toml, package.json, pyproject.toml, requirements.txt, go.mod.",
            repo_path.display()
        );
    } else {
        let mut indexed: u32 = 0;
        let mut symbol_counts: std::collections::HashMap<String, u32> =
            std::collections::HashMap::new();
        for lang in &langs {
            let blob = match scip::run_indexer(lang, &repo_path) {
                Ok(b) => b,
                Err(e) => {
                    eprintln!("[legion] skipped {repo} ({lang}): {e}");
                    continue;
                }
            };
            let hash = scip::content_hash(&blob);
            let now = chrono::Utc::now().to_rfc3339();
            let index = scip::ScipIndex {
                id: uuid::Uuid::now_v7().to_string(),
                repo: repo.clone(),
                lang: (*lang).to_string(),
                content_hash: hash,
                blob,
                updated_at: now,
                deleted_at: None,
            };
            let bytes_len = index.blob.len();
            let hash_prefix = &index.content_hash[..16];
            database.upsert_scip_index(&index)?;
            eprintln!("[legion] indexed {repo} ({lang}): {bytes_len} bytes, hash {hash_prefix}");
            indexed += 1;
            // Per-file symbol counts for the inventory (#705): a blob this
            // run just built failing to parse is not fatal to indexing, but
            // it must be loud -- counts silently stuck at 0 would misreport
            // every SCIP-covered file.
            match sym::symbol_counts_per_file(&index.blob) {
                Ok(counts) => symbol_counts.extend(counts),
                Err(e) => {
                    eprintln!(
                        "[legion] per-file symbol counts unavailable for {repo} ({lang}): {e}"
                    )
                }
            }
        }
        if indexed == 0 {
            return Err(error::LegionError::WatchConfig(format!(
                "no language indexed for {repo} -- every detected language ({}) failed; see warnings above",
                langs.join(", ")
            )));
        }
        if !symbol_counts.is_empty() {
            let updated = database.update_file_symbol_counts(&repo, &symbol_counts)?;
            eprintln!("[legion] symbol counts applied to {updated} inventoried files for {repo}");
        }
    }

    Ok(())
}

pub(crate) fn handle_sym(action: SymAction) -> error::Result<()> {
    let database = open_db()?;
    run_sym_action(&database, action)?;
    Ok(())
}

pub(crate) fn handle_reindex() -> error::Result<()> {
    let (database, index) = open_db_and_index()?;

    let reflections = database.get_all_for_reindex()?;
    let count = reflections.len();
    index.rebuild(&reflections)?;
    info!("[legion] reindexed {} reflections", count);
    Ok(())
}

pub(crate) fn handle_cleanup(retention_days: i64) -> error::Result<()> {
    let database = open_db()?;

    let result = database.cleanup_tombstones(retention_days)?;
    if result.is_empty() {
        eprintln!("[legion] no tombstones older than {} days", retention_days);
    } else {
        eprintln!(
            "[legion] cleaned up tombstones older than {} days: {} reflections, {} tasks, {} schedules ({} total)",
            retention_days,
            result.reflections,
            result.tasks,
            result.schedules,
            result.total()
        );
    }
    Ok(())
}

pub(crate) fn handle_rename(from: String, to: String) -> error::Result<()> {
    if from == to {
        eprintln!("[legion] source and destination are the same, nothing to do");
        return Ok(());
    }

    let (database, index) = open_db_and_index()?;

    let counts = database.rename_repo(&from, &to)?;
    eprintln!(
        "[legion] renamed '{}' -> '{}': {} reflections, {} tasks (from), {} tasks (to), {} board reads, {} watch handled, {} schedules",
        from,
        to,
        counts.reflections,
        counts.tasks_from,
        counts.tasks_to,
        counts.board_reads,
        counts.watch_handled,
        counts.schedules
    );

    // Reindex since repo name is in the search index
    let reflections = database.get_all_for_reindex()?;
    let reindex_count = reflections.len();
    index.rebuild(&reflections)?;
    eprintln!("[legion] reindexed {} reflections", reindex_count);

    // Update watch.toml
    let watch_path = data_dir()?.join("watch.toml");
    if watch::rename_in_config(&watch_path, &from, &to)? {
        eprintln!("[legion] updated watch.toml: '{}' -> '{}'", from, to);
    }

    eprintln!("[legion] total: {} rows updated", counts.total());
    Ok(())
}

#[cfg(test)]
mod index_banner_tests {
    use super::*;
    use chrono::{TimeZone, Utc};

    fn idx(repo: &str, lang: &str, updated_at: &str) -> scip::ScipIndex {
        scip::ScipIndex {
            id: "id".to_string(),
            repo: repo.to_string(),
            lang: lang.to_string(),
            content_hash: "h".to_string(),
            blob: vec![1, 2, 3],
            updated_at: updated_at.to_string(),
            deleted_at: None,
        }
    }

    #[test]
    fn empty_when_no_languages_detected() {
        let now = Utc.with_ymd_and_hms(2026, 5, 7, 12, 0, 0).unwrap();
        let banner = render_index_status_banner("legion", &[], &[], now);
        assert!(banner.is_empty());
    }

    #[test]
    fn one_line_summary_when_all_fresh() {
        let now = Utc.with_ymd_and_hms(2026, 5, 7, 12, 0, 0).unwrap();
        let recent = "2026-05-07T10:00:00Z";
        let indexes = vec![idx("legion", "rust", recent)];
        let banner = render_index_status_banner("legion", &["rust"], &indexes, now);
        assert!(banner.starts_with("[Legion] Index status for legion: "));
        assert!(banner.contains("rust: fresh"));
        assert_eq!(banner.lines().count(), 1, "fresh state must be one line");
    }

    #[test]
    fn loud_block_when_missing_language() {
        let now = Utc.with_ymd_and_hms(2026, 5, 7, 12, 0, 0).unwrap();
        let banner = render_index_status_banner("legion", &["rust", "typescript"], &[], now);
        assert!(banner.contains("[Legion] Index status for legion:"));
        assert!(banner.contains("rust: not indexed"));
        assert!(banner.contains("typescript: not indexed"));
        assert!(banner.contains("legion index legion"));
    }

    #[test]
    fn loud_block_when_stale() {
        let now = Utc.with_ymd_and_hms(2026, 5, 7, 12, 0, 0).unwrap();
        // 14 days ago -- past the 7-day threshold.
        let stale = "2026-04-23T12:00:00Z";
        let indexes = vec![idx("legion", "rust", stale)];
        let banner = render_index_status_banner("legion", &["rust"], &indexes, now);
        assert!(banner.contains("rust: STALE"));
        assert!(banner.contains("14d ago"));
        assert!(banner.contains("legion index legion"));
    }

    #[test]
    fn mixed_state_lists_all_languages_in_order() {
        let now = Utc.with_ymd_and_hms(2026, 5, 7, 12, 0, 0).unwrap();
        let recent = "2026-05-07T10:00:00Z"; // 2h ago
        let stale = "2026-04-23T12:00:00Z"; // 14d ago
        let indexes = vec![
            idx("legion", "rust", recent),
            idx("legion", "typescript", stale),
        ];
        let banner =
            render_index_status_banner("legion", &["rust", "typescript", "python"], &indexes, now);
        assert!(banner.contains("rust: fresh"));
        assert!(banner.contains("typescript: STALE"));
        assert!(banner.contains("python: not indexed"));
        // Multi-line block, not single line.
        assert!(banner.lines().count() > 2);
    }

    #[test]
    fn unparseable_timestamp_is_treated_as_stale() {
        let now = Utc.with_ymd_and_hms(2026, 5, 7, 12, 0, 0).unwrap();
        let indexes = vec![idx("legion", "rust", "not-a-timestamp")];
        let banner = render_index_status_banner("legion", &["rust"], &indexes, now);
        assert!(banner.contains("rust: STALE"));
        assert!(banner.contains("unknown age"));
    }

    #[test]
    fn age_humanized_into_appropriate_unit() {
        let now = Utc.with_ymd_and_hms(2026, 5, 7, 12, 0, 0).unwrap();
        // 90 minutes ago -> "1h ago"
        let one_hr_ago = "2026-05-07T10:30:00Z";
        let banner = render_index_status_banner(
            "legion",
            &["rust"],
            &[idx("legion", "rust", one_hr_ago)],
            now,
        );
        assert!(banner.contains("1h ago"));
    }
}
