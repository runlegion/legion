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
/// to "unknown" when its lookup fails, since a detached HEAD still has a valid
/// commit to key a quality gate on. Shared by the quality-gate recorder and
/// the `pr create` / `pr write-check` gate checks.
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
