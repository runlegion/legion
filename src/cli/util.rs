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

/// List files changed between main (or origin/main) and HEAD.
///
/// Uses `git diff -c core.quotePath=false --name-only main...HEAD` (three-dot
/// merge-base range) so that files changed only on main since the branch point
/// do not appear in the set. Falls back to `origin/main...HEAD` when `main` is
/// absent locally.
///
/// Before trying the diff, each candidate base ref is probed with
/// `git rev-parse --verify <ref>`. When a ref resolves but the diff command
/// still fails, that is a hard error (the environment is broken, not a
/// legitimate no-base situation). When neither ref resolves AND HEAD has no
/// parent commit (initial commit), an empty set is returned so the gate passes
/// vacuously. In any other failure mode this function returns `Err` so the
/// caller can refuse to record a gate rather than silently accepting vacuous
/// coverage.
///
/// Used by `legion quality-gate check` to build the coverage set the
/// articulation must address.
pub(crate) fn git_changed_files() -> Result<std::collections::HashSet<String>, error::LegionError> {
    // Probe whether git is available at all first.
    let probe = std::process::Command::new("git")
        .args(["rev-parse", "--is-inside-work-tree"])
        .output()
        .map_err(|e| {
            error::LegionError::WorkSource(format!("failed to run git (is it installed?): {e}"))
        })?;
    if !probe.status.success() {
        return Err(error::LegionError::WorkSource(
            "git rev-parse --is-inside-work-tree failed -- not inside a git repo".to_owned(),
        ));
    }

    // Probe each candidate base ref. Collect the ones that resolve.
    let candidates: [&str; 2] = ["main", "origin/main"];
    let mut resolved_base: Option<&str> = None;
    for candidate in candidates {
        let verify = std::process::Command::new("git")
            .args(["rev-parse", "--verify", candidate])
            .output()
            .map_err(|e| {
                error::LegionError::WorkSource(format!(
                    "failed to probe git ref '{candidate}': {e}"
                ))
            })?;
        if verify.status.success() {
            resolved_base = Some(candidate);
            break;
        }
    }

    let Some(base_ref) = resolved_base else {
        // Neither main nor origin/main exists. This is legitimate only when
        // HEAD itself has no parent (initial commit / orphan branch). Probe
        // for a parent commit; if there is one, the branch just has an unusual
        // base-ref name, which is a hard error rather than vacuous-pass.
        let parent = std::process::Command::new("git")
            .args(["rev-parse", "--verify", "HEAD~1"])
            .output()
            .map_err(|e| error::LegionError::WorkSource(format!("failed to probe HEAD~1: {e}")))?;
        if parent.status.success() {
            // HEAD has a parent but no known base ref -- refuse rather than
            // vacuously pass. An agent running on an unusual default branch
            // (master, develop, etc.) must configure the base ref.
            return Err(error::LegionError::WorkSource(
                "could not find base ref 'main' or 'origin/main' and HEAD has a parent commit; \
                 the simplify gate cannot determine the changed-file set. \
                 Ensure 'main' or 'origin/main' exists in this repo."
                    .to_owned(),
            ));
        }
        // HEAD has no parent: genuine initial commit, vacuously valid. This is
        // unreachable in the normal branch-off-main workflow (HEAD always has
        // a parent once `main` exists), but note it on stderr rather than
        // passing silently -- a vacuous pass should never be invisible to
        // whoever is watching the gate run.
        eprintln!(
            "[legion] no base ref ('main' or 'origin/main') found and HEAD has no parent \
             commit -- treating this as the initial commit. The changed-file set is empty \
             and any articulation is accepted vacuously."
        );
        return Ok(std::collections::HashSet::new());
    };

    // Run the three-dot diff with core.quotePath=false so non-ASCII paths
    // are returned as-is (no octal escaping) and match heading paths exactly.
    let refspec = format!("{base_ref}...HEAD");
    let out = std::process::Command::new("git")
        .args([
            "-c",
            "core.quotePath=false",
            "diff",
            "--name-only",
            &refspec,
        ])
        .output()
        .map_err(|e| error::LegionError::WorkSource(format!("failed to run git diff: {e}")))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        return Err(error::LegionError::WorkSource(format!(
            "git diff --name-only {refspec} failed (base ref '{base_ref}' resolved \
             but diff did not): {stderr}"
        )));
    }

    let set: std::collections::HashSet<String> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_owned)
        .collect();
    Ok(set)
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
