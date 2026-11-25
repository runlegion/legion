//! Shared CLI helpers: db/index opening, audit writes, stdin/file input,
//! git HEAD lookup, age formatting (carved from main.rs, #610).

use crate::cli::datadir::data_dir;
use crate::{db, error, search};

/// Open the legion database at the canonical data dir.
///
/// The one place the `<data_dir>/legion.db` path is constructed in this
/// file. Handlers that also need the search index use
/// `open_db_and_index`; handlers that need the data dir for anything
/// beyond the database keep calling `data_dir()` themselves.
pub(crate) fn open_db() -> error::Result<db::Database> {
    let base = data_dir()?;
    db::Database::open(&base.join("legion.db"))
}

/// Open the legion database and the Tantivy search index together.
///
/// Companion to `open_db` for the handlers that write reflections (every
/// reflection insert must hit both stores or search silently diverges
/// from the database).
pub(crate) fn open_db_and_index() -> error::Result<(db::Database, search::SearchIndex)> {
    let base = data_dir()?;
    let database = db::Database::open(&base.join("legion.db"))?;
    let index = search::SearchIndex::open(&base.join("index"))?;
    Ok((database, index))
}

/// Best-effort audit log entry. Insert failures warn to stderr, never
/// abort -- but the caller supplies the connection, so an arm that cannot
/// open the database fails loudly up front instead of silently dropping
/// its audit trail one row at a time (#610).
pub(crate) fn audit(database: &db::Database, input: &db::AuditInput<'_>) {
    if let Err(e) = database.insert_audit_entry(input) {
        eprintln!("[legion] warning: audit log failed: {}", e);
    }
}

/// Raise the soft file-descriptor limit to the hard limit.
///
/// macOS ships a low soft limit (often 2560) which Tantivy can exhaust
/// when opening index segments. The hard limit is much higher (or unlimited).
/// This is a no-op on failure -- the worst case is the original limit.
pub(crate) fn raise_fd_limit() {
    match rlimit::increase_nofile_limit(u64::MAX) {
        Ok(_) => {}
        Err(e) => eprintln!("[legion] warning: could not raise fd limit: {e}"),
    }
}

/// Read the current HEAD commit hash and branch name. Errors if `git
/// rev-parse HEAD` fails (not a git repo / no commits); the branch falls back
/// to "unknown" when its lookup fails. On a detached HEAD the branch is the
/// literal `HEAD`, which is not a valid key for quality-gate findings:
/// `quality-gate record` and `check` refuse it (#1403). Shared by the
/// quality-gate recorder and the `pr create` / `pr write-check` gate checks.
pub(crate) fn git_head_commit_and_branch() -> Result<(String, String), error::LegionError> {
    let head = std::process::Command::new("git")
        .args(["rev-parse", "HEAD"])
        .output()
        .map_err(|e| error::LegionError::WorkSource(format!("failed to read git HEAD: {e}")))?;
    if !head.status.success() {
        return Err(error::LegionError::WorkSource(
            "git rev-parse HEAD failed -- is this a git repo?".to_owned(),
        ));
    }
    let commit_hash = String::from_utf8_lossy(&head.stdout).trim().to_owned();

    let branch_out = std::process::Command::new("git")
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .output()
        .map_err(|e| error::LegionError::WorkSource(format!("failed to read git branch: {e}")))?;
    let branch = if branch_out.status.success() {
        String::from_utf8_lossy(&branch_out.stdout)
            .trim()
            .to_owned()
    } else {
        "unknown".to_owned()
    };
    Ok((commit_hash, branch))
}

/// The repo key for a gate record (#1403): the registered repo the process
/// cwd lies in (#1479; a linked worktree maps to its main checkout's repo).
/// `LEGION_REPO` is deliberately not consulted: the commit hash comes from the
/// cwd's git, so the repo key must too.
pub(crate) fn current_repo_name() -> Result<String, error::LegionError> {
    crate::watch::registered_repo_for_cwd().ok_or_else(|| {
        error::LegionError::WorkSource(format!(
            "this directory is not in a registered repo; a gate cannot be recorded without one. \
             {REGISTER_HINT}"
        ))
    })
}

/// What to do when a command needs a repo and the working directory is not
/// in a registered one (#1479).
pub(crate) const REGISTER_HINT: &str = "Run it from a registered repo (or one of its worktrees), pass --repo <name>, \
     or register this checkout with `legion watch add <path> --name <name>`.";

/// Read input from `path` (the value of a `--*-file` flag) or, when it is
/// `None`, from stdin. `flag` names the originating flag for the error
/// message. Shared by the pr-write and verify gates, which both accept their
/// payload as either a file or a stdin pipe.
pub(crate) fn read_file_or_stdin(
    path: Option<&str>,
    flag: &str,
) -> Result<String, error::LegionError> {
    match path {
        Some(p) => std::fs::read_to_string(p)
            .map_err(|e| error::LegionError::WorkSource(format!("failed to read {flag} {p}: {e}"))),
        None => {
            use std::io::Read as _;
            let mut buf = String::new();
            std::io::stdin().read_to_string(&mut buf).map_err(|e| {
                error::LegionError::WorkSource(format!("failed to read stdin: {e}"))
            })?;
            Ok(buf)
        }
    }
}

/// Resolve a prose body from an inline flag value or stdin (#917). Returns
/// the inline value when present; otherwise reads stdin -- but fails fast
/// with a clear error when stdin is a terminal instead of blocking on EOF,
/// so a forgotten flag in an interactive call does not hang. The piped/
/// redirected form is the only shell-safe one: backticks/$() in an inline
/// double-quoted arg are command-substituted before legion runs.
///
/// Also refuses a blank result (empty, or whitespace-only once trimmed)
/// regardless of where it came from -- one check here means `comment`,
/// `commit`, `reflect`, and `post` never each carry their own copy of it.
pub(crate) fn inline_or_stdin(
    inline: Option<String>,
    flag: &str,
) -> Result<String, error::LegionError> {
    let resolved: String = match inline {
        Some(value) => value,
        None => {
            use std::io::IsTerminal as _;
            if std::io::stdin().is_terminal() {
                return Err(error::LegionError::WorkSource(format!(
                    "{flag} not provided and stdin is a terminal -- pass {flag} or pipe the \
                     body on stdin"
                )));
            }
            read_file_or_stdin(None, flag)?
        }
    };
    if resolved.trim().is_empty() {
        return Err(error::LegionError::WorkSource(format!(
            "{flag} is empty -- pass a non-empty value or pipe non-empty content on stdin"
        )));
    }
    Ok(resolved)
}

/// [`inline_or_stdin`] for a verb that also has a mutually-exclusive
/// alternative body source (`reflect`/`post` `--transcript`, `commit`
/// `--message-file`): fall to stdin ONLY when the inline value and the
/// alternative are both absent, so an explicit alternative is never
/// shadowed by a stray stdin pipe. Returns `inline` unchanged when the
/// alternative is present, so the caller's existing three-way resolution
/// still sees a `None` it must reconcile with the alternative.
pub(crate) fn inline_or_stdin_unless(
    inline: Option<String>,
    alternative_present: bool,
    flag: &str,
) -> Result<Option<String>, error::LegionError> {
    if inline.is_none() && !alternative_present {
        Ok(Some(inline_or_stdin(None, flag)?))
    } else {
        Ok(inline)
    }
}

/// Read `pipe` line by line, relaying each line to our own stderr as it
/// arrives and accumulating it into the returned string.
///
/// Shared by `legion push` and `legion commit`: both shell out to a git
/// command that can sit behind a long-running hook (the nested-claude
/// pre-push review), and both need that
/// output visible AS IT HAPPENS as well as captured for the failure
/// message. Buffering until exit turns a two-minute hook into what looks
/// like a hang.
pub(crate) fn relay_and_capture_stderr(pipe: impl std::io::Read) -> String {
    use std::io::BufRead;
    let reader = std::io::BufReader::new(pipe);
    let mut captured = String::new();
    for line in reader.lines() {
        match line {
            Ok(l) => {
                eprintln!("{l}");
                captured.push_str(&l);
                captured.push('\n');
            }
            Err(_) => break,
        }
    }
    captured
}

pub(crate) fn format_age(d: std::time::Duration) -> String {
    let secs = d.as_secs();
    if secs < 60 {
        format!("{}s", secs)
    } else if secs < 3600 {
        format!("{}m", secs / 60)
    } else if secs < 86400 {
        format!("{}h", secs / 3600)
    } else {
        format!("{}d", secs / 86400)
    }
}
