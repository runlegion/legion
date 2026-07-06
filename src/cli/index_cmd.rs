//! `legion index`/`sym`/`reindex`/`cleanup`/`rename` handlers and the
//! background indexer plumbing (carved from main.rs, #610).

use std::path::{Path, PathBuf};

use clap::Subcommand;

use crate::cli::datadir::data_dir;
use crate::cli::util::{open_db, open_db_and_index};
use crate::{css, db, error, etc, graph, inventory, scip, sym, telemetry, watch};

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

    /// Non-symbol answer surface (#704): query shapes over the files SCIP
    /// does not parse (docs, configs, css, prose). `sym` answers symbol
    /// questions; `sym etc` answers everything else.
    Etc {
        #[command(subcommand)]
        shape: EtcShape,
    },

    /// Structured, cross-repo view of the file inventory built by `legion
    /// index` -- the "build tree" shape (#706), the sanctioned replacement
    /// for `find` / `ls -R` / `tree` / a throwaway `os.walk`. Queries
    /// `Database::list_file_inventory`; never walks the filesystem at
    /// query time, so it answers instantly regardless of repo size.
    Tree {
        /// Restrict to a single repo (default: cross-repo over every repo
        /// with inventory rows, each entry tagged with its own `repo` field)
        #[arg(long)]
        repo: Option<String>,
        /// Filter by extension, without the leading dot (e.g. "rs")
        #[arg(long)]
        ext: Option<String>,
        /// Scope to a subtree prefix (e.g. "src/db")
        #[arg(long)]
        under: Option<String>,
        /// Max path depth: number of path segments, counted from --under
        /// when given, else from the repo root
        #[arg(long)]
        depth: Option<u32>,
        /// Emit results as a JSON array of structured entries
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

#[derive(Subcommand)]
pub(crate) enum EtcShape {
    /// Exact, line-accurate content search over watched repos -- the
    /// sanctioned grep (#707). Direct disk scan: always fresh, true grep
    /// parity including regex and punctuation-heavy literals.
    FindContent {
        /// Pattern to search: a regex by default, a literal with --fixed-strings.
        /// Hyphen-leading patterns (`--spacing-0.5`) are accepted as values,
        /// not parsed as flags -- they are a headline query shape (#707).
        #[arg(allow_hyphen_values = true)]
        pattern: String,
        /// Restrict to a single repo (default: every repo in watch.toml)
        #[arg(long)]
        repo: Option<String>,
        /// Restrict to files with this extension (without the dot)
        #[arg(long)]
        ext: Option<String>,
        /// Treat the pattern as a literal string, not a regex
        #[arg(long)]
        fixed_strings: bool,
        /// Search hidden files and directories (.github/, .claude/, dotfiles).
        /// `.git/` stays excluded. Mirrors ripgrep's --hidden. NOTE: gitignore
        /// is the only guard here -- a secret-bearing dotfile that is not
        /// gitignored (e.g. an un-ignored .env) WILL be searched and its
        /// matching lines printed.
        #[arg(long)]
        hidden: bool,
        /// Emit results as a JSON array of ContentHit objects
        #[arg(long)]
        json: bool,
    },

    /// Return one field from a JSON/TOML/YAML file, or the YAML frontmatter
    /// of a `.md`/`.mdx`/`.astro` file, without reading the whole file
    /// (#708). The bytes an agent wants -- a config value, a frontmatter
    /// field -- not the surrounding file.
    Extract {
        /// Path to the structured file to extract from.
        path: PathBuf,
        /// jq-style dotted path: `.`-separated segments walk objects, and a
        /// purely numeric segment indexes an array (`scripts.build`,
        /// `keywords.0`, `workspaces.packages.1`). Keys that themselves
        /// contain a literal `.` are out of scope for v1.
        #[arg(long)]
        field: String,
        /// Emit the value as JSON instead of a bare scalar/lines
        #[arg(long)]
        json: bool,
    },

    /// Locate a file by basename/glob or by role heuristic across watched
    /// repos (#709) -- the "which repo owns X" / "locate the Y checkout"
    /// shape. Queries the file inventory built by `legion index`
    /// (`Database::list_file_inventory`); never walks the filesystem at
    /// query time.
    FindFile {
        /// Basename or glob to match (e.g. "components.json",
        /// "*.test.ts"). Matched against the file's basename; a query
        /// containing `/` matches the full repo-relative path instead
        /// (e.g. "src/db/*.rs"). `*`/`?` are glob wildcards; case-sensitive.
        query: String,
        /// Restrict to a single repo (default: cross-repo over every repo
        /// with inventory rows, each entry tagged with its own `repo` field)
        #[arg(long)]
        repo: Option<String>,
        /// Restrict to files matching a coarse role heuristic inferred
        /// from path/extension only -- no content read.
        #[arg(long)]
        role: Option<db::inventory::FileRole>,
        /// Emit results as a JSON array of FileInventoryEntry objects
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
        SymAction::Tree {
            repo,
            ext,
            under,
            depth,
            json,
        } => run_sym_tree(database, repo, ext, under, depth, json),
        SymAction::Etc { shape } => run_sym_etc(database, shape),
    }
}

/// Dispatch `legion sym etc <shape>`. `find-content`/`extract` do not read
/// the SCIP store -- find-content scans watched workdirs directly and
/// extract reads one file directly. `find-file` queries the file inventory
/// table, hence the `database` handle every arm now threads through.
fn run_sym_etc(database: &db::Database, shape: EtcShape) -> error::Result<()> {
    match shape {
        EtcShape::FindContent {
            pattern,
            repo,
            ext,
            fixed_strings,
            hidden,
            json,
        } => run_etc_find_content(&pattern, repo, ext, fixed_strings, hidden, json),
        EtcShape::Extract { path, field, json } => run_etc_extract(&path, &field, json),
        EtcShape::FindFile {
            query,
            repo,
            role,
            json,
        } => run_etc_find_file(database, &query, repo, role, json),
    }
}

/// `sym etc find-content` (#707): exact content search over watch.toml
/// workdirs via the in-process ripgrep engine. Prints `path:line: text`
/// (repo-prefixed when scanning cross-repo) or a JSON hit array; suppressed,
/// skipped, and binary counts go to stderr so truncation is never silent,
/// and a repo whose workdir cannot be walked is named. Telemetry records one
/// row per invocation -- error exits (empty corpus, unknown repo, invalid
/// regex, unscannable corpus) carry the error text so the epic's metric can
/// separate "tool answered zero" from "tool failed to answer" (#704).
fn run_etc_find_content(
    pattern: &str,
    repo: Option<String>,
    ext: Option<String>,
    fixed_strings: bool,
    hidden: bool,
    json: bool,
) -> error::Result<()> {
    let scan = scan_etc_content(
        pattern,
        repo.as_deref(),
        ext.as_deref(),
        fixed_strings,
        hidden,
    );

    let usage = telemetry::EtcUsageRecord {
        ts: chrono::Utc::now(),
        command: "find-content".to_string(),
        repo: repo.clone(),
        pattern: pattern.to_string(),
        fixed_strings,
        hit_count: scan.as_ref().map_or(0, |(r, _)| r.hits.len() as u64),
        skipped_files: scan.as_ref().map_or(0, |(r, _)| r.skipped_files),
        error: scan.as_ref().err().map(|e| e.to_string()),
        failed_repos: scan
            .as_ref()
            .map_or(0, |(r, _)| r.failed_repos.len() as u64),
        format: None,
    };
    if let Err(e) = telemetry::append_etc_usage(&usage) {
        eprintln!("[legion] etc usage telemetry write failed: {e}");
    }

    let (result, repo_count) = scan?;
    if json {
        println!("{}", serde_json::to_string(&result.hits)?);
    } else {
        let cross_repo = repo.is_none() && repo_count > 1;
        for hit in &result.hits {
            if cross_repo {
                println!("{}/{}:{}: {}", hit.repo, hit.path, hit.line, hit.text);
            } else {
                println!("{}:{}: {}", hit.path, hit.line, hit.text);
            }
        }
    }
    for (name, err) in &result.failed_repos {
        eprintln!("[legion] repo '{name}' could not be scanned: {err}");
    }
    if result.suppressed > 0 {
        eprintln!(
            "[legion] {} more matches suppressed (cap {}); narrow with --repo/--ext or a tighter pattern",
            result.suppressed,
            etc::MAX_HITS
        );
    }
    if result.skipped_files > 0 {
        eprintln!(
            "[legion] {} files skipped (unreadable or larger than {} bytes)",
            result.skipped_files,
            etc::MAX_FILE_SIZE
        );
    }
    if result.binary_skipped > 0 {
        eprintln!(
            "[legion] {} binary files skipped (NUL byte found)",
            result.binary_skipped
        );
    }
    Ok(())
}

/// `sym etc extract` (#708): pull one field out of a config/frontmatter file
/// without reading the whole thing. Telemetry records one row per
/// invocation -- `hit_count` is 1 on a resolved field and 0 on a miss or
/// error, so the epic's zero-result metric covers this shape too. `format`
/// is detected independently of the extract result (cheap: an extension
/// check, no extra file read) so a usage row still names the format even
/// when the field itself was missing.
fn run_etc_extract(path: &Path, field: &str, json: bool) -> error::Result<()> {
    let outcome = etc::extract_field(path, field);
    let format = etc::detect_format(path)
        .ok()
        .map(|f| f.as_str().to_string());

    let usage = telemetry::EtcUsageRecord {
        ts: chrono::Utc::now(),
        command: "extract".to_string(),
        repo: None,
        pattern: field.to_string(),
        fixed_strings: false,
        hit_count: u64::from(outcome.is_ok()),
        skipped_files: 0,
        error: outcome.as_ref().err().map(|e| e.to_string()),
        failed_repos: 0,
        format,
    };
    if let Err(e) = telemetry::append_etc_usage(&usage) {
        eprintln!("[legion] etc usage telemetry write failed: {e}");
    }

    let value = outcome?;
    if json {
        println!("{}", serde_json::to_string(&value)?);
    } else {
        print_extract_value(&value);
    }
    Ok(())
}

/// Print an extracted value the way an agent wants to consume it without
/// `--json`: a string prints bare (no quotes), an array prints each element
/// on its own line (flattening nested arrays the same way), and every other
/// JSON scalar (number/bool/null) or a whole object prints via its compact
/// JSON form.
fn print_extract_value(value: &serde_json::Value) {
    match value {
        serde_json::Value::String(s) => println!("{s}"),
        serde_json::Value::Array(items) => {
            for item in items {
                print_extract_value(item);
            }
        }
        other => println!("{other}"),
    }
}

/// Resolve the scan scope from watch.toml and run the search. Split from
/// `run_etc_find_content` so every exit -- including the loud errors below --
/// funnels through the caller's telemetry write. Returns the result plus the
/// number of repos scanned (for cross-repo output prefixing).
fn scan_etc_content(
    pattern: &str,
    repo: Option<&str>,
    ext: Option<&str>,
    fixed_strings: bool,
    hidden: bool,
) -> error::Result<(etc::FindContentResult, usize)> {
    let base = data_dir()?;
    let watch_path = base.join("watch.toml");
    let all = watch::list_repos_in_config(&watch_path)?;
    // An empty corpus must be loud: zero hits over zero repos is
    // indistinguishable from "the pattern is not in your code".
    if all.is_empty() {
        return Err(error::LegionError::WatchConfig(
            "no repos in watch.toml -- nothing to search. Add one with `legion watch add <name> <path>`."
                .to_string(),
        ));
    }
    let repos: Vec<(String, PathBuf)> = match repo {
        Some(name) => {
            let entry = all.iter().find(|r| r.name == name).ok_or_else(|| {
                error::LegionError::WatchConfig(format!(
                    "repo '{name}' not in watch.toml. Add it with `legion watch add {name} <path>`."
                ))
            })?;
            vec![(entry.name.clone(), PathBuf::from(&entry.workdir))]
        }
        None => all
            .iter()
            .map(|r| (r.name.clone(), PathBuf::from(&r.workdir)))
            .collect(),
    };

    let scope = etc::ContentScope {
        repos: &repos,
        ext,
        fixed_strings,
        include_hidden: hidden,
        max_file_size: etc::MAX_FILE_SIZE,
        max_hits: etc::MAX_HITS,
    };
    let repo_count = repos.len();
    let result = etc::find_content(pattern, &scope)?;
    // Every scoped repo failing to walk is the empty-corpus case in
    // disguise: zero hits that mean "nothing was searched", not "no match".
    if result.failed_repos.len() == repo_count {
        let detail: Vec<String> = result
            .failed_repos
            .iter()
            .map(|(name, err)| format!("{name}: {err}"))
            .collect();
        return Err(error::LegionError::Search(format!(
            "no repo could be scanned -- {}. Fix the workdir(s) or `legion watch remove` stale entries.",
            detail.join("; ")
        )));
    }
    Ok((result, repo_count))
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

/// One row in `sym tree`'s structured output: the file-inventory columns
/// the issue names (repo, path, ext, lang, size, symbol_count). `mtime` is
/// deliberately dropped -- `sym tree` answers "what/where", not "when did
/// it last change".
#[derive(Debug, serde::Serialize)]
struct SymTreeEntry {
    repo: String,
    path: String,
    ext: Option<String>,
    lang: Option<String>,
    size: u64,
    symbol_count: u32,
}

impl From<db::inventory::FileInventoryEntry> for SymTreeEntry {
    fn from(e: db::inventory::FileInventoryEntry) -> Self {
        Self {
            repo: e.repo,
            path: e.path,
            ext: e.ext,
            lang: e.lang,
            size: e.size,
            symbol_count: e.symbol_count,
        }
    }
}

/// Per-repo freshness metadata attached to a `--json` response (#746): when
/// the backing inventory snapshot was captured, and whether the repo's
/// current HEAD (read live, once per distinct repo represented in the
/// result -- one `git rev-parse HEAD`, not a filesystem walk) has moved
/// since.
#[derive(Debug, serde::Serialize)]
struct SnapshotFreshness {
    repo: String,
    indexed_at: Option<String>,
    head_at_index: Option<String>,
    current_head: Option<String>,
    /// True only when both heads are `Some` and differ.
    head_drift: bool,
}

/// `--json` envelope replacing the previous bare-array output for both
/// `sym tree` and `sym etc find-file` (#746). This deliberately breaks the
/// previous bare-array `--json` shape (0.19.0 shipped it days before this
/// issue landed, with no external contract on it) -- freshness metadata
/// cannot be attached to a bare array.
#[derive(Debug, serde::Serialize)]
struct FreshJsonEnvelope<T: serde::Serialize> {
    snapshots: Vec<SnapshotFreshness>,
    entries: Vec<T>,
}

/// True only when `head_at_index` and `current_head` are both present and
/// differ -- nothing to compare (either side `None`) is not drift (#746).
fn head_drift(head_at_index: Option<&str>, current_head: Option<&str>) -> bool {
    matches!((head_at_index, current_head), (Some(a), Some(b)) if a != b)
}

/// Repo scope for a freshness computation: explicit `--repo` always yields
/// exactly that one name (even when the result set is empty); cross-repo
/// yields the distinct repos actually represented in `entry_repos`, in
/// first-seen order (callers already sort entries by `(repo, path)`, so
/// this comes out repo-sorted for free). An empty cross-repo result yields
/// an empty list, not one entry per watch.toml repo (#746).
fn freshness_repo_scope<'a>(
    repo: Option<&str>,
    entry_repos: impl Iterator<Item = &'a str>,
) -> Vec<String> {
    match repo {
        Some(name) => vec![name.to_string()],
        None => {
            let mut seen = std::collections::HashSet::new();
            entry_repos
                .filter(|r| seen.insert((*r).to_string()))
                .map(|r| r.to_string())
                .collect()
        }
    }
}

/// Resolve `name`'s workdir from an already-loaded watch.toml repo list, or
/// `None` when it is not a known repo -- a stale/removed inventory row must
/// not error the whole freshness computation (#746).
fn resolve_repo_workdir(repos: &[watch::WatchRepoConfig], name: &str) -> Option<PathBuf> {
    repos
        .iter()
        .find(|r| r.name == name)
        .map(|r| PathBuf::from(&r.workdir))
}

/// Compute freshness metadata for one `--json`/human freshness line per repo
/// in `freshness_repo_scope`'s result: read the stored `inventory_snapshots`
/// row, resolve the repo's live workdir, and do one `git rev-parse HEAD`
/// (never a filesystem walk) to detect drift (#746). Loads watch.toml once
/// up front rather than per repo -- cross-repo results with many distinct
/// repos must not reparse the file once per name.
fn compute_freshness<'a>(
    database: &db::Database,
    repo: Option<&str>,
    entry_repos: impl Iterator<Item = &'a str>,
) -> error::Result<Vec<SnapshotFreshness>> {
    let names = freshness_repo_scope(repo, entry_repos);
    let base = data_dir()?;
    let watch_path = base.join("watch.toml");
    let all_repos = watch::list_repos_in_config(&watch_path)?;

    let mut result = Vec::with_capacity(names.len());
    for name in names {
        let snapshot = database.get_inventory_snapshot(&name)?;
        let indexed_at = snapshot.as_ref().map(|s| s.indexed_at.clone());
        let head_at_index = snapshot.and_then(|s| s.head);
        let current_head = resolve_repo_workdir(&all_repos, &name)
            .as_deref()
            .and_then(inventory::current_head);
        let drift = head_drift(head_at_index.as_deref(), current_head.as_deref());
        result.push(SnapshotFreshness {
            repo: name,
            indexed_at,
            head_at_index,
            current_head,
            head_drift: drift,
        });
    }
    Ok(result)
}

/// Shorten a full SHA to git's conventional 7-char display form. Shorter
/// input (should not happen for a real SHA) is returned as-is. `get`
/// (byte-index-checked) rather than a raw slice: a real git SHA is
/// all-ASCII hex so a 7-byte boundary is always a char boundary, but this
/// reads a plain `TEXT` column with no format constraint enforced at the
/// database layer, so no code path here may assume it can't panic.
fn short_sha(sha: &str) -> &str {
    sha.get(..7).unwrap_or(sha)
}

/// Coarse "how long ago" label for a freshness line: seconds/minutes/
/// hours/days, whichever unit `elapsed` falls into. Negative or malformed
/// durations (clock skew) floor to zero rather than printing a negative
/// number.
fn format_relative_duration(elapsed: chrono::Duration) -> String {
    let secs = elapsed.num_seconds().max(0);
    if secs < 60 {
        format!("{secs}s ago")
    } else if secs < 3600 {
        format!("{}m ago", secs / 60)
    } else if secs < 86_400 {
        format!("{}h ago", secs / 3600)
    } else {
        format!("{}d ago", secs / 86_400)
    }
}

/// Human-facing freshness line for one repo, printed to stderr before the
/// entry table (#746):
/// - no snapshot recorded: hint to re-index.
/// - snapshot present, HEAD matches or nothing to compare: "up to date".
/// - snapshot present, HEAD drifted: a loud warning naming both HEADs.
fn format_freshness_line(s: &SnapshotFreshness, now: chrono::DateTime<chrono::Utc>) -> String {
    let Some(indexed_at) = &s.indexed_at else {
        return format!(
            "{}: no inventory snapshot recorded; re-run 'legion index {}' to capture indexed-at/HEAD",
            s.repo, s.repo
        );
    };
    let ago = chrono::DateTime::parse_from_rfc3339(indexed_at)
        .map(|d| format_relative_duration(now - d.with_timezone(&chrono::Utc)))
        .unwrap_or_else(|_| "unknown".to_string());
    let head_disp = s.head_at_index.as_deref().map(short_sha).unwrap_or("none");
    if s.head_drift {
        let current_disp = s.current_head.as_deref().map(short_sha).unwrap_or("none");
        format!(
            "{}: indexed {indexed_at} ({ago}), HEAD {head_disp} -- WARNING: current HEAD is \
             {current_disp}; inventory may be stale, re-run 'legion index {}'",
            s.repo, s.repo
        )
    } else {
        format!(
            "{}: indexed {indexed_at} ({ago}), HEAD {head_disp} -- up to date",
            s.repo
        )
    }
}

/// `sym tree` (#706): a structured, cross-repo view of the file inventory
/// (`Database::list_file_inventory`) -- no filesystem walk at query time.
/// `--repo` omitted means cross-repo over whatever the inventory table
/// holds, each entry tagged with its own `repo` field. `--ext` narrows
/// server-side via the DB filter; `--under`/`--depth` narrow in-process
/// since neither has a dedicated column. Telemetry records every
/// invocation -- including error exits -- with the result count, mirroring
/// `find-content` (#704's primary metric is the zero-result rate across
/// every sanctioned query shape).
fn run_sym_tree(
    database: &db::Database,
    repo: Option<String>,
    ext: Option<String>,
    under: Option<String>,
    depth: Option<u32>,
    json: bool,
) -> error::Result<()> {
    use std::io::Write;

    let scan = scan_tree(
        database,
        repo.as_deref(),
        ext.as_deref(),
        under.as_deref(),
        depth,
    );

    let usage = telemetry::EtcUsageRecord {
        ts: chrono::Utc::now(),
        command: "tree".to_string(),
        repo: repo.clone(),
        pattern: describe_tree_scope(ext.as_deref(), under.as_deref(), depth),
        fixed_strings: false,
        hit_count: scan.as_ref().map_or(0, |r| r.entries.len() as u64),
        skipped_files: 0,
        error: scan.as_ref().err().map(|e| e.to_string()),
        failed_repos: 0,
        format: None,
    };
    if let Err(e) = telemetry::append_etc_usage(&usage) {
        eprintln!("[legion] etc usage telemetry write failed: {e}");
    }

    let result = scan?;
    if let Some(msg) = &result.message {
        eprintln!("[legion] {msg}");
    }

    let snapshots = compute_freshness(
        database,
        repo.as_deref(),
        result.entries.iter().map(|e| e.repo.as_str()),
    )?;

    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    if json {
        let envelope = FreshJsonEnvelope {
            snapshots,
            entries: result.entries,
        };
        serde_json::to_writer(&mut out, &envelope)?;
        writeln!(out)?;
    } else {
        let now = chrono::Utc::now();
        for s in &snapshots {
            eprintln!("[legion] {}", format_freshness_line(s, now));
        }
        let cross_repo = repo.is_none();
        for e in &result.entries {
            let ext_col = e.ext.as_deref().unwrap_or("-");
            let lang_col = e.lang.as_deref().unwrap_or("-");
            if cross_repo {
                writeln!(
                    out,
                    "{}/{}\t{ext_col}\t{lang_col}\t{}\t{}",
                    e.repo, e.path, e.size, e.symbol_count
                )?;
            } else {
                writeln!(
                    out,
                    "{}\t{ext_col}\t{lang_col}\t{}\t{}",
                    e.path, e.size, e.symbol_count
                )?;
            }
        }
    }
    Ok(())
}

/// Result of a `sym tree` scan: the filtered, sorted entries plus an
/// optional human-facing note for the empty case (either "never indexed"
/// or "filter matched nothing", distinguished by `compute_uncovered_message`).
struct TreeScan {
    entries: Vec<SymTreeEntry>,
    message: Option<String>,
}

/// Resolve and filter `sym tree` scope: validate an explicit `--repo`
/// against watch.toml (the fix-hint error path), query the inventory table
/// (server-side `--ext`/`--repo` filter), then narrow by `--under`/`--depth`
/// in-process. Cross-repo (`repo: None`) never touches watch.toml -- the
/// inventory table is the source of truth once `legion index` has run.
fn scan_tree(
    database: &db::Database,
    repo: Option<&str>,
    ext: Option<&str>,
    under: Option<&str>,
    depth: Option<u32>,
) -> error::Result<TreeScan> {
    if let Some(name) = repo {
        validate_repo_known(name)?;
    }

    let filter = db::inventory::InventoryFilter {
        repo,
        ext,
        lang: None,
    };
    let raw = database.list_file_inventory(&filter)?;
    let entries = filter_and_scope(raw, under, depth);
    let message = compute_uncovered_message(database, repo, &entries)?;
    Ok(TreeScan { entries, message })
}

/// Error when `name` is not a repo in watch.toml -- the fix hint the issue
/// requires for an unknown `--repo`.
fn validate_repo_known(name: &str) -> error::Result<()> {
    let base = data_dir()?;
    let watch_path = base.join("watch.toml");
    let all = watch::list_repos_in_config(&watch_path)?;
    if !all.iter().any(|r| r.name == name) {
        return Err(error::LegionError::WatchConfig(format!(
            "repo '{name}' not in watch.toml. Add it with `legion watch add {name} <path>`."
        )));
    }
    Ok(())
}

/// Narrow `raw` by `--under` (subtree prefix) and `--depth` (max path
/// segments), then sort by `(repo, path)`. Pure function of already-fetched
/// rows so it is unit-testable without a database.
fn filter_and_scope(
    raw: Vec<db::inventory::FileInventoryEntry>,
    under: Option<&str>,
    depth: Option<u32>,
) -> Vec<SymTreeEntry> {
    let mut entries: Vec<SymTreeEntry> = raw
        .into_iter()
        .filter(|e| under.is_none_or(|u| under_matches(&e.path, u)))
        .filter(|e| depth.is_none_or(|d| tree_depth(&e.path, under) <= d as usize))
        .map(SymTreeEntry::from)
        .collect();
    entries.sort_by(|a, b| a.repo.cmp(&b.repo).then(a.path.cmp(&b.path)));
    entries
}

/// True when `path` is `under` itself or lives inside it. Boundary-checked
/// so `src/db` matches `src/db/x.rs` but not the sibling `src/dbfoo.rs`.
fn under_matches(path: &str, under: &str) -> bool {
    let u = under.trim_end_matches('/');
    if u.is_empty() {
        return true;
    }
    path == u || path.starts_with(&format!("{u}/"))
}

/// Path depth in segments, counted relative to `under` when given (so
/// `--under src/db --depth 1` returns only the direct children of
/// `src/db`), else relative to the repo root.
fn tree_depth(path: &str, under: Option<&str>) -> usize {
    let rel = match under {
        Some(u) => {
            let u = u.trim_end_matches('/');
            path.strip_prefix(u)
                .map(|s| s.trim_start_matches('/'))
                .unwrap_or(path)
        }
        None => path,
    };
    if rel.is_empty() {
        0
    } else {
        rel.split('/').count()
    }
}

/// Empty-result message, distinguishing "this repo has never been indexed"
/// from "the filter matched nothing" -- the criterion requires the former
/// to be an explicit "run `legion index <repo>`" hint, not silence. Runs a
/// second, unfiltered-by-ext query only on the empty path, so the common
/// non-empty case pays no extra cost.
fn compute_uncovered_message(
    database: &db::Database,
    repo: Option<&str>,
    entries: &[SymTreeEntry],
) -> error::Result<Option<String>> {
    if !entries.is_empty() {
        return Ok(None);
    }
    let baseline = database.list_file_inventory(&db::inventory::InventoryFilter {
        repo,
        ext: None,
        lang: None,
    })?;
    Ok(Some(if baseline.is_empty() {
        match repo {
            Some(name) => format!("no inventory for '{name}'; run `legion index {name}` first"),
            None => {
                "no inventory across any watched repo; run `legion index <repo>` first".to_string()
            }
        }
    } else {
        "no files match the --ext/--under/--depth filter".to_string()
    }))
}

/// Compact telemetry description of the filters used, e.g. "ext=rs,under=src/db,depth=2".
fn describe_tree_scope(ext: Option<&str>, under: Option<&str>, depth: Option<u32>) -> String {
    let mut parts = Vec::new();
    if let Some(e) = ext {
        parts.push(format!("ext={e}"));
    }
    if let Some(u) = under {
        parts.push(format!("under={u}"));
    }
    if let Some(d) = depth {
        parts.push(format!("depth={d}"));
    }
    parts.join(",")
}

/// `sym etc find-file` (#709): locate a file by basename/glob or role
/// heuristic across the file inventory (`Database::list_file_inventory`) --
/// no filesystem walk at query time. Telemetry records one row per
/// invocation, mirroring `find-content`/`tree` (#704's zero-result metric).
fn run_etc_find_file(
    database: &db::Database,
    query: &str,
    repo: Option<String>,
    role: Option<db::inventory::FileRole>,
    json: bool,
) -> error::Result<()> {
    use std::io::Write;

    let scan = scan_find_file(database, repo.as_deref(), query, role);

    let usage = telemetry::EtcUsageRecord {
        ts: chrono::Utc::now(),
        command: "find-file".to_string(),
        repo: repo.clone(),
        pattern: describe_find_file_scope(query, role),
        fixed_strings: false,
        hit_count: scan.as_ref().map_or(0, |r| r.entries.len() as u64),
        skipped_files: 0,
        error: scan.as_ref().err().map(|e| e.to_string()),
        failed_repos: 0,
        format: None,
    };
    if let Err(e) = telemetry::append_etc_usage(&usage) {
        eprintln!("[legion] etc usage telemetry write failed: {e}");
    }

    let result = scan?;
    if let Some(msg) = &result.message {
        eprintln!("[legion] {msg}");
    }

    let snapshots = compute_freshness(
        database,
        repo.as_deref(),
        result.entries.iter().map(|e| e.repo.as_str()),
    )?;

    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    if json {
        let envelope = FreshJsonEnvelope {
            snapshots,
            entries: result.entries,
        };
        serde_json::to_writer(&mut out, &envelope)?;
        writeln!(out)?;
    } else {
        let now = chrono::Utc::now();
        for s in &snapshots {
            eprintln!("[legion] {}", format_freshness_line(s, now));
        }
        let cross_repo = repo.is_none();
        for e in &result.entries {
            if cross_repo {
                writeln!(out, "{}/{}", e.repo, e.path)?;
            } else {
                writeln!(out, "{}", e.path)?;
            }
        }
    }
    Ok(())
}

/// Result of a `sym etc find-file` scan: the filtered, sorted entries plus
/// an optional human-facing note for the empty case (either "never
/// indexed" or "name/role matched nothing").
#[derive(Debug)]
struct FindFileScan {
    entries: Vec<db::inventory::FileInventoryEntry>,
    message: Option<String>,
}

/// Resolve and filter `sym etc find-file` scope: validate an explicit
/// `--repo` against watch.toml (the fix-hint error path), query the
/// inventory table once (server-side `--repo` filter only -- name and role
/// have no dedicated column), then narrow by name/role in-process, the
/// same split `scan_tree` uses for `--under`/`--depth`. Unlike `scan_tree`
/// (whose main query is already `--ext`-filtered, so its uncovered-message
/// baseline is a genuinely different, unfiltered query), find-file's main
/// query has no server-side name/role filter to begin with -- so `raw`
/// emptiness *is* the "never indexed" baseline, and no second query is
/// needed to compute it.
fn scan_find_file(
    database: &db::Database,
    repo: Option<&str>,
    query: &str,
    role: Option<db::inventory::FileRole>,
) -> error::Result<FindFileScan> {
    if let Some(name) = repo {
        validate_repo_known(name)?;
    }

    let filter = db::inventory::InventoryFilter {
        repo,
        ext: None,
        lang: None,
    };
    let raw = database.list_file_inventory(&filter)?;
    let never_indexed = raw.is_empty();
    let mut entries: Vec<db::inventory::FileInventoryEntry> = raw
        .into_iter()
        .filter(|e| db::inventory::matches_name(e, query))
        .filter(|e| role.is_none_or(|r| r.matches(e)))
        .collect();
    entries.sort_by(|a, b| a.repo.cmp(&b.repo).then(a.path.cmp(&b.path)));

    let message = compute_find_file_uncovered_message(repo, query, role, never_indexed, &entries);
    Ok(FindFileScan { entries, message })
}

/// Empty-result message, distinguishing "this repo has never been indexed"
/// (or no repo has, cross-repo) from "the name/role filter matched
/// nothing" -- the criterion requires an explicit hint, not a bare empty
/// list. `never_indexed` is the emptiness of the *unfiltered* inventory
/// read `scan_find_file` already performed -- a pure function of that
/// result, not a second database round-trip.
fn compute_find_file_uncovered_message(
    repo: Option<&str>,
    query: &str,
    role: Option<db::inventory::FileRole>,
    never_indexed: bool,
    entries: &[db::inventory::FileInventoryEntry],
) -> Option<String> {
    if !entries.is_empty() {
        return None;
    }
    Some(if never_indexed {
        match repo {
            Some(name) => format!("no inventory for '{name}'; run `legion index {name}` first"),
            None => {
                "no inventory across any watched repo; run `legion index <repo>` first".to_string()
            }
        }
    } else {
        match role {
            Some(r) => format!("no file named '{query}' with role '{r}' found"),
            None => format!("no file named '{query}' found"),
        }
    })
}

/// Compact telemetry description of the query/role used, e.g.
/// "query=components.json,role=config".
fn describe_find_file_scope(query: &str, role: Option<db::inventory::FileRole>) -> String {
    match role {
        Some(r) => format!("query={query},role={r}"),
        None => format!("query={query}"),
    }
}

#[cfg(test)]
mod find_file_tests {
    use super::*;

    fn entry(
        repo: &str,
        path: &str,
        ext: Option<&str>,
        lang: Option<&str>,
    ) -> db::inventory::FileInventoryEntry {
        db::inventory::FileInventoryEntry {
            repo: repo.to_string(),
            path: path.to_string(),
            ext: ext.map(|s| s.to_string()),
            lang: lang.map(|s| s.to_string()),
            size: 10,
            mtime: "2026-07-01T00:00:00+00:00".to_string(),
            symbol_count: 0,
        }
    }

    #[test]
    fn scan_find_file_matches_by_basename_across_repos() {
        let db = crate::db::testutil::test_db();
        db.upsert_file_inventory(&[
            entry("alpha", "web/components.json", Some("json"), None),
            entry("beta", "components.json", Some("json"), None),
            entry("alpha", "src/main.rs", Some("rs"), Some("rust")),
        ])
        .unwrap();

        let scan = scan_find_file(&db, None, "components.json", None).unwrap();
        assert_eq!(scan.entries.len(), 2);
        assert!(scan.entries.iter().any(|e| e.repo == "alpha"));
        assert!(scan.entries.iter().any(|e| e.repo == "beta"));
        assert!(scan.message.is_none());
    }

    // NOTE: repo-scoping (`--repo alpha`) is not unit-tested here because
    // `scan_find_file` validates `--repo` against a real watch.toml via
    // `validate_repo_known`, which needs `LEGION_DATA_DIR` wired up --
    // covered by the CLI end-to-end tests in tests/integration/find_file.rs
    // instead (same split `sym_tree_tests` uses for `scan_tree`).

    #[test]
    fn scan_find_file_filters_by_role() {
        let db = crate::db::testutil::test_db();
        db.upsert_file_inventory(&[
            entry("r", "app.config.json", Some("json"), None),
            entry("r", "app.rs", Some("rs"), Some("rust")),
        ])
        .unwrap();

        let scan = scan_find_file(&db, None, "*", Some(db::inventory::FileRole::Config)).unwrap();
        assert_eq!(scan.entries.len(), 1);
        assert_eq!(scan.entries[0].path, "app.config.json");
    }

    #[test]
    fn scan_find_file_unknown_repo_errors() {
        let db = crate::db::testutil::test_db();
        let err = scan_find_file(&db, Some("ghost-repo"), "x", None).unwrap_err();
        assert!(err.to_string().contains("ghost-repo"));
    }

    #[test]
    fn message_none_when_entries_present_find_file() {
        let entries = vec![entry("r", "components.json", Some("json"), None)];
        let msg = compute_find_file_uncovered_message(
            Some("r"),
            "components.json",
            None,
            false,
            &entries,
        );
        assert!(msg.is_none());
    }

    #[test]
    fn message_no_inventory_hint_when_repo_never_indexed() {
        let msg = compute_find_file_uncovered_message(Some("ghost"), "x", None, true, &[]);
        assert!(msg.as_deref().is_some_and(
            |m| m.contains("no inventory for 'ghost'") && m.contains("legion index ghost")
        ));
    }

    #[test]
    fn message_cross_repo_wording_when_repo_none_find_file() {
        let msg = compute_find_file_uncovered_message(None, "x", None, true, &[]);
        assert!(
            msg.as_deref()
                .is_some_and(|m| m.contains("no inventory across any watched repo"))
        );
    }

    #[test]
    fn message_no_match_names_query_and_role() {
        let msg = compute_find_file_uncovered_message(
            Some("r"),
            "nope.json",
            Some(db::inventory::FileRole::Config),
            false,
            &[],
        );
        assert!(msg.as_deref().is_some_and(|m| {
            m.contains("no file named 'nope.json'") && m.contains("role 'config'")
        }));
    }

    #[test]
    fn describe_find_file_scope_query_only() {
        assert_eq!(
            describe_find_file_scope("components.json", None),
            "query=components.json"
        );
    }

    #[test]
    fn describe_find_file_scope_with_role() {
        assert_eq!(
            describe_find_file_scope("*", Some(db::inventory::FileRole::Config)),
            "query=*,role=config"
        );
    }
}

#[cfg(test)]
mod sym_tree_tests {
    use super::*;

    fn entry(
        repo: &str,
        path: &str,
        ext: Option<&str>,
        lang: Option<&str>,
    ) -> db::inventory::FileInventoryEntry {
        db::inventory::FileInventoryEntry {
            repo: repo.to_string(),
            path: path.to_string(),
            ext: ext.map(|s| s.to_string()),
            lang: lang.map(|s| s.to_string()),
            size: 10,
            mtime: "2026-07-01T00:00:00+00:00".to_string(),
            symbol_count: 0,
        }
    }

    // --- under_matches ---

    #[test]
    fn under_matches_exact_and_nested() {
        assert!(under_matches("src/db", "src/db"));
        assert!(under_matches("src/db/inventory.rs", "src/db"));
        assert!(under_matches("src/db/sub/x.rs", "src/db"));
    }

    #[test]
    fn under_matches_rejects_sibling_prefix() {
        // "src/dbfoo.rs" shares the string prefix "src/db" but is not
        // inside it -- the boundary check must reject it.
        assert!(!under_matches("src/dbfoo.rs", "src/db"));
        assert!(!under_matches("src/other/x.rs", "src/db"));
    }

    #[test]
    fn under_matches_trailing_slash_is_normalized() {
        assert!(under_matches("src/db/x.rs", "src/db/"));
    }

    #[test]
    fn under_matches_empty_matches_everything() {
        assert!(under_matches("anything.rs", ""));
    }

    // --- tree_depth ---

    #[test]
    fn tree_depth_relative_to_repo_root() {
        assert_eq!(tree_depth("a.rs", None), 1);
        assert_eq!(tree_depth("src/a.rs", None), 2);
        assert_eq!(tree_depth("src/db/a.rs", None), 3);
    }

    #[test]
    fn tree_depth_relative_to_under() {
        assert_eq!(tree_depth("src/db/a.rs", Some("src/db")), 1);
        assert_eq!(tree_depth("src/db/sub/a.rs", Some("src/db")), 2);
        // The under path itself: zero remaining segments.
        assert_eq!(tree_depth("src/db", Some("src/db")), 0);
    }

    // --- filter_and_scope ---

    #[test]
    fn filter_and_scope_no_filters_returns_all_sorted() {
        let raw = vec![
            entry("r", "b.rs", Some("rs"), Some("rust")),
            entry("r", "a.rs", Some("rs"), Some("rust")),
        ];
        let got = filter_and_scope(raw, None, None);
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].path, "a.rs");
        assert_eq!(got[1].path, "b.rs");
    }

    #[test]
    fn filter_and_scope_sorts_by_repo_then_path() {
        let raw = vec![
            entry("beta", "a.rs", Some("rs"), Some("rust")),
            entry("alpha", "z.rs", Some("rs"), Some("rust")),
        ];
        let got = filter_and_scope(raw, None, None);
        assert_eq!(got[0].repo, "alpha");
        assert_eq!(got[1].repo, "beta");
    }

    #[test]
    fn filter_and_scope_under_scopes_subtree() {
        let raw = vec![
            entry("r", "src/db/inventory.rs", Some("rs"), Some("rust")),
            entry("r", "src/other.rs", Some("rs"), Some("rust")),
            entry("r", "src/dbfoo.rs", Some("rs"), Some("rust")),
        ];
        let got = filter_and_scope(raw, Some("src/db"), None);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].path, "src/db/inventory.rs");
    }

    #[test]
    fn filter_and_scope_depth_limits_results() {
        let raw = vec![
            entry("r", "src/a.rs", Some("rs"), Some("rust")),
            entry("r", "src/sub/b.rs", Some("rs"), Some("rust")),
        ];
        let got = filter_and_scope(raw, Some("src"), Some(1));
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].path, "src/a.rs");
    }

    #[test]
    fn filter_and_scope_non_symbol_file_has_lang_none() {
        let raw = vec![entry("r", "README.md", Some("md"), None)];
        let got = filter_and_scope(raw, None, None);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].lang, None);
    }

    // --- compute_uncovered_message ---

    #[test]
    fn message_none_when_entries_present() {
        let db = crate::db::testutil::test_db();
        let entries = vec![SymTreeEntry {
            repo: "r".to_string(),
            path: "a.rs".to_string(),
            ext: Some("rs".to_string()),
            lang: Some("rust".to_string()),
            size: 1,
            symbol_count: 0,
        }];
        let msg = compute_uncovered_message(&db, Some("r"), &entries).unwrap();
        assert!(msg.is_none());
    }

    #[test]
    fn message_no_inventory_when_repo_never_indexed() {
        let db = crate::db::testutil::test_db();
        let msg = compute_uncovered_message(&db, Some("ghost"), &[]).unwrap();
        assert!(msg.as_deref().is_some_and(
            |m| m.contains("no inventory for 'ghost'") && m.contains("legion index ghost")
        ));
    }

    #[test]
    fn message_cross_repo_wording_when_repo_none() {
        let db = crate::db::testutil::test_db();
        let msg = compute_uncovered_message(&db, None, &[]).unwrap();
        assert!(
            msg.as_deref()
                .is_some_and(|m| m.contains("no inventory across any watched repo"))
        );
    }

    #[test]
    fn message_no_match_when_repo_has_rows_but_filter_empty() {
        let db = crate::db::testutil::test_db();
        db.upsert_file_inventory(&[entry("r", "a.rs", Some("rs"), Some("rust"))])
            .unwrap();
        let msg = compute_uncovered_message(&db, Some("r"), &[]).unwrap();
        assert!(
            msg.as_deref()
                .is_some_and(|m| m.contains("no files match the --ext/--under/--depth filter"))
        );
    }

    // --- describe_tree_scope ---

    #[test]
    fn describe_tree_scope_empty_when_no_filters() {
        assert_eq!(describe_tree_scope(None, None, None), "");
    }

    #[test]
    fn describe_tree_scope_all_filters() {
        assert_eq!(
            describe_tree_scope(Some("rs"), Some("src/db"), Some(2)),
            "ext=rs,under=src/db,depth=2"
        );
    }
}

#[cfg(test)]
mod freshness_tests {
    use super::*;

    // --- head_drift ---

    #[test]
    fn snapshot_freshness_head_drift_true_only_when_both_present_and_differ() {
        assert!(head_drift(Some("aaa"), Some("bbb")), "both present, differ");
        assert!(!head_drift(Some("aaa"), Some("aaa")), "both present, match");
        assert!(!head_drift(None, Some("bbb")), "no index-time head");
        assert!(!head_drift(Some("aaa"), None), "no current head");
        assert!(!head_drift(None, None), "neither known");
    }

    // --- freshness_repo_scope ---

    #[test]
    fn freshness_repo_scope_explicit_repo_is_always_one_entry_even_when_empty() {
        let scope = freshness_repo_scope(Some("r"), std::iter::empty());
        assert_eq!(scope, vec!["r".to_string()]);
    }

    #[test]
    fn freshness_repo_scope_cross_repo_dedupes_in_first_seen_order() {
        let repos = vec!["beta", "alpha", "beta", "alpha"];
        let scope = freshness_repo_scope(None, repos.into_iter());
        assert_eq!(scope, vec!["beta".to_string(), "alpha".to_string()]);
    }

    #[test]
    fn freshness_repo_scope_cross_repo_empty_result_is_empty_scope() {
        let scope = freshness_repo_scope(None, std::iter::empty());
        assert!(scope.is_empty());
    }

    // --- short_sha ---

    #[test]
    fn short_sha_truncates_to_seven_chars() {
        assert_eq!(short_sha("8ed18c6abcdef1234567890"), "8ed18c6");
    }

    #[test]
    fn short_sha_returns_shorter_input_as_is() {
        assert_eq!(short_sha("abc"), "abc");
    }

    // --- format_relative_duration ---

    #[test]
    fn format_relative_duration_buckets_by_unit() {
        assert_eq!(
            format_relative_duration(chrono::Duration::seconds(30)),
            "30s ago"
        );
        assert_eq!(
            format_relative_duration(chrono::Duration::minutes(14)),
            "14m ago"
        );
        assert_eq!(
            format_relative_duration(chrono::Duration::hours(5)),
            "5h ago"
        );
        assert_eq!(
            format_relative_duration(chrono::Duration::days(6)),
            "6d ago"
        );
    }

    #[test]
    fn format_relative_duration_negative_floors_to_zero() {
        assert_eq!(
            format_relative_duration(chrono::Duration::seconds(-5)),
            "0s ago"
        );
    }

    // --- format_freshness_line ---

    #[test]
    fn format_freshness_line_no_snapshot_recorded_hint() {
        let s = SnapshotFreshness {
            repo: "legion".to_string(),
            indexed_at: None,
            head_at_index: None,
            current_head: None,
            head_drift: false,
        };
        let line = format_freshness_line(&s, chrono::Utc::now());
        assert!(
            line.contains("no inventory snapshot recorded") && line.contains("legion index legion"),
            "got: {line}"
        );
    }

    #[test]
    fn format_freshness_line_up_to_date_when_no_drift() {
        let now = chrono::Utc::now();
        let s = SnapshotFreshness {
            repo: "legion".to_string(),
            indexed_at: Some(now.to_rfc3339()),
            head_at_index: Some("8ed18c6ffff".to_string()),
            current_head: Some("8ed18c6ffff".to_string()),
            head_drift: false,
        };
        let line = format_freshness_line(&s, now);
        assert!(line.contains("up to date"), "got: {line}");
        assert!(!line.contains("WARNING"), "got: {line}");
    }

    #[test]
    fn format_freshness_line_warns_on_drift() {
        let now = chrono::Utc::now();
        let s = SnapshotFreshness {
            repo: "legion".to_string(),
            indexed_at: Some(now.to_rfc3339()),
            head_at_index: Some("8ed18c6ffff".to_string()),
            current_head: Some("31b01ecffff".to_string()),
            head_drift: true,
        };
        let line = format_freshness_line(&s, now);
        assert!(
            line.contains("WARNING: current HEAD is 31b01ec")
                && line.contains("re-run 'legion index legion'"),
            "got: {line}"
        );
    }
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
    let repo_path = PathBuf::from(&entry.workdir);
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

    // Coverage guarantee (#713): a detected-but-unindexed language is either
    // "not indexed yet" (running `legion index` fixes it) or "indexer
    // unavailable" (the binary the language needs is not on this machine's
    // PATH, so `legion index` cannot fix it by itself). Computed here, not
    // inside the pure renderer below, so the renderer stays deterministic
    // and testable without mocking PATH.
    let unavailable_langs: Vec<&str> = detected
        .iter()
        .filter(|lang| !indexes.iter().any(|i| i.lang == **lang) && !scip::indexer_available(lang))
        .copied()
        .collect();

    let now = chrono::Utc::now();
    let banner =
        render_index_status_banner(repo_name, &detected, &indexes, &unavailable_langs, now);
    if !banner.is_empty() {
        writeln!(out, "{banner}")?;
    }
    Ok(())
}

/// Pure renderer for the SessionStart index status banner. Empty string
/// means "silent" (every detected language has a fresh index). Returns a
/// multi-line block when anything is stale or missing.
///
/// `unavailable_langs` names detected languages whose indexer binary is not
/// on `PATH` (#713 coverage guarantee) -- computed by the caller via
/// `scip::indexer_available` so this renderer stays a pure function of its
/// arguments and does not read the environment itself.
///
/// Stale threshold: 7 days. Override-friendly through a build constant if
/// future tuning needs it; not exposed as a CLI flag in v1.
fn render_index_status_banner(
    repo_name: &str,
    detected_langs: &[&str],
    indexes: &[scip::ScipIndex],
    unavailable_langs: &[&str],
    now: chrono::DateTime<chrono::Utc>,
) -> String {
    const STALE_THRESHOLD_DAYS: i64 = 7;

    if detected_langs.is_empty() {
        // No supported language detected -- silent. Avoids cluttering
        // SessionStart for repos that legion does not index.
        return String::new();
    }

    enum LangHealth {
        Fresh {
            lang: String,
            age: String,
        },
        Stale {
            lang: String,
            age: String,
        },
        Missing {
            lang: String,
        },
        /// #713: detected but no indexer binary is installed for it. Distinct
        /// from `Missing` -- `legion index` cannot fix this by itself.
        IndexerUnavailable {
            lang: String,
            binary: String,
        },
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
            None => {
                if unavailable_langs.contains(lang) {
                    report.push(LangHealth::IndexerUnavailable {
                        lang: (*lang).to_string(),
                        binary: scip::indexer_binary_hint(lang).to_string(),
                    });
                } else {
                    report.push(LangHealth::Missing {
                        lang: (*lang).to_string(),
                    });
                }
            }
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
                    "  {lang}: not indexed yet -- run `legion index {repo_name}` to build"
                ));
            }
            LangHealth::IndexerUnavailable { lang, binary } => {
                lines.push(format!(
                    "  {lang}: indexer unavailable ({binary} not on PATH) -- `legion index` cannot cover this language until it is installed; `legion sym etc find-content` / `sym tree` / `sym etc find-file` still cover its files by content and structure without SCIP"
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
    let repo_path = PathBuf::from(&entry.workdir);

    // Serialize concurrent index runs on this repo: an older run's stale
    // live-paths walk snapshot must not prune inventory rows a newer
    // overlapping run just inserted (#722, PR #718 review). Held across
    // the rest of this function -- walk, upsert, prune, and the SCIP
    // indexing loop -- and released when `_index_lock` drops at return.
    let _index_lock = watch::acquire_index_lock(&base, &repo)?;

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
    let live_paths: Vec<&str> = outcome.entries.iter().map(|e| e.path.as_str()).collect();
    database.upsert_file_inventory(&outcome.entries)?;
    // Snapshot fact (#746): when this walk ran and the repo's HEAD at that
    // moment, so `sym tree`/`sym etc find-file` can surface staleness.
    // `current_head` is best-effort (never an error) -- a non-git workdir
    // still gets a snapshot row, with `head: None`.
    let indexed_at = chrono::Utc::now().to_rfc3339();
    let head_at_index = inventory::current_head(&repo_path);
    database.upsert_inventory_snapshot(&repo, &indexed_at, head_at_index.as_deref())?;
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

    // Module graph (#710): parse js/ts/jsx/tsx imports and resolve each
    // specifier against its referrer. Runs over the inventory's
    // typescript-lang subset (extension-based, from the walk above) --
    // independent of the SCIP `langs` marker-file detection below, since
    // oxc_parser/oxc_resolver need only the files themselves, not a
    // scip-typescript binary on PATH.
    let ts_files: Vec<PathBuf> = outcome
        .entries
        .iter()
        .filter(|e| e.lang.as_deref() == Some("typescript"))
        .map(|e| PathBuf::from(&e.path))
        .collect();
    // Computed once and reused for both the upsert below (which files must
    // have their stale rows cleared before the fresh edges are inserted)
    // and the prune below (which files are still live at all).
    let live_from: Vec<&str> = ts_files.iter().filter_map(|p| p.to_str()).collect();
    let mut edge_count: usize = 0;
    if !ts_files.is_empty() {
        match graph::build_module_graph(&repo, &repo_path, &ts_files) {
            Ok(edges) => {
                edge_count = edges.len();
                database.upsert_module_edges(&repo, &live_from, &edges)?;
            }
            Err(e) => {
                eprintln!("[legion] module graph build failed for {repo}: {e}");
            }
        }
    }
    // Prune unconditionally on a clean walk -- mirroring the file-inventory
    // prune above, including its zero-entries case: a repo that has gone to
    // zero JS/TS files (last one deleted, or the repo migrated off JS) must
    // still wipe its now-stale module_edges rows, exactly as
    // `prune_file_inventory` wipes the whole repo when `live_paths` is empty.
    // Gating this behind `!ts_files.is_empty()` would leave those rows
    // orphaned forever. Same partial-walk guard as the file-inventory prune:
    // a walk with errors may be missing files that still exist, and pruning
    // against that incomplete set would evict edges for files the walk
    // simply failed to see this run.
    if outcome.walk_errors == 0 {
        let pruned_edges = database.prune_module_edges(&repo, &live_from)?;
        eprintln!(
            "[legion] module graph: {edge_count} edges for {repo} ({pruned_edges} stale edges pruned)"
        );
    } else {
        eprintln!(
            "[legion] module graph: {edge_count} edges for {repo} (stale-edge prune skipped: partial walk)"
        );
    }

    // CSS symbols (#711): parse compiled stylesheets' class-selector and
    // custom-property definitions via lightningcss, so `sym` can answer
    // "where is `.foo` / `--token` defined" for the design system. Runs over
    // the inventory's `.css`-extension subset -- css is not a SCIP language
    // (`inventory::lang_for_ext` maps it to `None`), so this is independent
    // of the `langs` marker-file detection below.
    //
    // `counts_available`/`symbol_counts` are shared with the SCIP loop below
    // and enriched by BOTH sources before the single `update_file_symbol_counts`
    // call at the end of this function: that call resets the whole repo's
    // counts to 0 before applying the map, so calling it twice (once per
    // source) would let the second call zero out the first source's counts.
    let mut counts_available = false;
    let mut symbol_counts: std::collections::HashMap<String, u32> =
        std::collections::HashMap::new();

    let css_files: Vec<PathBuf> = outcome
        .entries
        .iter()
        .filter(|e| e.ext.as_deref() == Some("css"))
        .map(|e| PathBuf::from(&e.path))
        .collect();
    let live_css: Vec<&str> = css_files.iter().filter_map(|p| p.to_str()).collect();
    let mut css_symbols: Vec<css::CssSymbol> = Vec::new();
    if !css_files.is_empty() {
        css_symbols = css::extract_css_symbols_for_files(&repo_path, &css_files);
        counts_available = true;
        for s in &css_symbols {
            *symbol_counts.entry(s.path.clone()).or_insert(0) += 1;
        }
        database.upsert_css_symbols(&repo, &live_css, &css_symbols)?;
    }
    // Prune unconditionally on a clean walk -- mirroring the module-graph
    // prune above (and `prune_file_inventory`), including its zero-entries
    // case: a repo that has gone to zero css files (last one deleted) must
    // still wipe its now-stale css_symbols rows. Gating this behind
    // `!css_files.is_empty()` would leave those rows orphaned forever.
    if outcome.walk_errors == 0 {
        let pruned_css = database.prune_css_symbols(&repo, &live_css)?;
        eprintln!(
            "[legion] css symbols: {} for {repo} ({pruned_css} stale rows pruned)",
            css_symbols.len()
        );
    } else {
        eprintln!(
            "[legion] css symbols: {} for {repo} (stale-row prune skipped: partial walk)",
            css_symbols.len()
        );
    }

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
                Ok(counts) => {
                    counts_available = true;
                    symbol_counts.extend(counts);
                }
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
    }

    // Gate on a source having run at all, not on non-empty counts: a blob
    // (or css batch) that parsed but yielded zero definitions (the repo's
    // last definition was deleted) must still run the enrich pass so its
    // reset clears the now-stale counts. A source that never ran (no
    // languages detected, no css files) leaves prior enrichment untouched --
    // this call is shared by SCIP and CSS (see the comment above the css
    // block) so a docs-only-turned-css-only repo still gets counts applied.
    if counts_available {
        let updated = database.update_file_symbol_counts(&repo, &symbol_counts)?;
        eprintln!("[legion] symbol counts applied to {updated} inventoried files for {repo}");
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
        let banner = render_index_status_banner("legion", &[], &[], &[], now);
        assert!(banner.is_empty());
    }

    #[test]
    fn one_line_summary_when_all_fresh() {
        let now = Utc.with_ymd_and_hms(2026, 5, 7, 12, 0, 0).unwrap();
        let recent = "2026-05-07T10:00:00Z";
        let indexes = vec![idx("legion", "rust", recent)];
        let banner = render_index_status_banner("legion", &["rust"], &indexes, &[], now);
        assert!(banner.starts_with("[Legion] Index status for legion: "));
        assert!(banner.contains("rust: fresh"));
        assert_eq!(banner.lines().count(), 1, "fresh state must be one line");
    }

    #[test]
    fn loud_block_when_missing_language() {
        let now = Utc.with_ymd_and_hms(2026, 5, 7, 12, 0, 0).unwrap();
        let banner = render_index_status_banner("legion", &["rust", "typescript"], &[], &[], now);
        assert!(banner.contains("[Legion] Index status for legion:"));
        assert!(banner.contains("rust: not indexed yet"));
        assert!(banner.contains("typescript: not indexed yet"));
        assert!(banner.contains("legion index legion"));
    }

    /// #713 coverage guarantee: a detected language whose indexer binary is
    /// not on PATH must be named as "indexer unavailable", not lumped in
    /// with "not indexed yet" -- the two demand different operator actions,
    /// and collapsing them is what taught agents "sym doesn't do X" instead
    /// of "nobody installed X's indexer here".
    #[test]
    fn loud_block_distinguishes_indexer_unavailable_from_not_indexed_yet() {
        let now = Utc.with_ymd_and_hms(2026, 5, 7, 12, 0, 0).unwrap();
        let banner =
            render_index_status_banner("legion", &["rust", "python"], &[], &["python"], now);
        assert!(
            banner.contains("rust: not indexed yet"),
            "rust (available indexer, just unindexed) must read 'not indexed yet': {banner}"
        );
        assert!(
            banner.contains("python: indexer unavailable (scip-python not on PATH)"),
            "python (no indexer on PATH) must name the missing binary: {banner}"
        );
        assert!(
            banner.contains("sym etc find-content"),
            "unavailable-indexer message must point at sym etc as the language-agnostic fallback: {banner}"
        );
    }

    #[test]
    fn loud_block_when_stale() {
        let now = Utc.with_ymd_and_hms(2026, 5, 7, 12, 0, 0).unwrap();
        // 14 days ago -- past the 7-day threshold.
        let stale = "2026-04-23T12:00:00Z";
        let indexes = vec![idx("legion", "rust", stale)];
        let banner = render_index_status_banner("legion", &["rust"], &indexes, &[], now);
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
        let banner = render_index_status_banner(
            "legion",
            &["rust", "typescript", "python"],
            &indexes,
            &[],
            now,
        );
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
        let banner = render_index_status_banner("legion", &["rust"], &indexes, &[], now);
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
            &[],
            now,
        );
        assert!(banner.contains("1h ago"));
    }
}
