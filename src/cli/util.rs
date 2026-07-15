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

/// Similarity threshold pinned on every `git diff -M` invocation in
/// [`git_changed_files`] (#779). Passed explicitly on the command line so the
/// coverage set never drifts with a repo's (or a contributor's global)
/// `diff.renames` config -- an ambient config change must not silently widen
/// or narrow what the simplify gate treats as a "pure" rename. 50% matches
/// git's own long-standing default for `-M` with no argument.
const RENAME_SIMILARITY: &str = "-M50%";

/// The changed-file set resolved by [`git_changed_files`], plus the audit
/// trail the R100 auto-clear (#779) requires.
pub(crate) struct ChangedFiles {
    /// Paths the simplify articulation must cover: every added/modified/
    /// deleted/type-changed path, the new-path side of any rename with a
    /// content delta (`R<100`), and both sides of an undetected rename
    /// (below [`RENAME_SIMILARITY`], which git reports as a plain delete +
    /// add pair rather than a single `R` line).
    pub(crate) files: std::collections::HashSet<String>,
    /// `(old_path, new_path)` pairs for every pure (`R100`, zero-delta)
    /// rename excluded from `files`. Git's own rename detection guarantees
    /// these carry no simplification risk -- content is byte-identical, only
    /// the path moved -- but the pairs are surfaced here (rather than simply
    /// dropped) so the exclusion is auditable, not silent (field case: a
    /// 509-file strangler PR where 406 were verbatim moves).
    pub(crate) cleared_renames: Vec<(String, String)>,
    /// The base ref the diff actually ran against: the resolved `main` /
    /// `origin/main` fallback, the caller's explicit `--base` override, or
    /// `None` in the vacuous initial-commit case where no diff ran at all.
    /// Recorded on the gate row so a too-narrow base stays visible in the
    /// audit trail rather than disappearing once the coverage set looks
    /// clean (#779).
    pub(crate) base: Option<String>,
}

/// List files changed between `base` (or, when `None`, `main`/`origin/main`)
/// and HEAD, auto-clearing pure renames from the coverage set.
///
/// Uses `git diff -c core.quotePath=false --name-status -M50% <base>...HEAD`
/// (three-dot merge-base range, explicit similarity pin -- see
/// [`RENAME_SIMILARITY`]) so that files changed only on the base ref since the
/// branch point do not appear in the set. When `base` is `None`, falls back
/// to `origin/main...HEAD` when `main` is absent locally (the pre-#779
/// default-resolution behavior, unchanged).
///
/// When `base` is `Some`, it is probed with `git rev-parse --verify <ref>`
/// and any failure to resolve is a hard error -- an explicit `--base` that
/// does not name a real ref must never silently fall back to the default
/// resolution or vacuously pass, matching the existing no-base-ref behavior
/// for the auto-detected case. When `base` is `None` and neither `main` nor
/// `origin/main` resolves AND HEAD has no parent commit (initial commit), an
/// empty set is returned so the gate passes vacuously. In any other failure
/// mode this function returns `Err` so the caller can refuse to record a gate
/// rather than silently accepting vacuous coverage.
///
/// `--name-status` (rather than `--name-only`) is what makes the R100
/// auto-clear possible: it reports renames as `R<similarity>\t<old>\t<new>`
/// instead of collapsing a detected rename down to just the new path, so a
/// zero-delta (`R100`) move can be told apart from one that also touched
/// content (`R<100`).
///
/// Used by `legion quality-gate check` to build the coverage set the
/// articulation must address.
pub(crate) fn git_changed_files(base: Option<&str>) -> Result<ChangedFiles, error::LegionError> {
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

    let resolved_base: Option<String> = if let Some(explicit) = base {
        // An explicit --base must resolve to a real ref. No fallback, no
        // vacuous pass -- a typo'd or unmerged base ref is a hard error so
        // the gate never runs against an accidentally empty diff.
        let verify = std::process::Command::new("git")
            .args(["rev-parse", "--verify", explicit])
            .output()
            .map_err(|e| {
                error::LegionError::WorkSource(format!(
                    "failed to probe --base ref '{explicit}': {e}"
                ))
            })?;
        if !verify.status.success() {
            let stderr = String::from_utf8_lossy(&verify.stderr);
            return Err(error::LegionError::WorkSource(format!(
                "--base ref '{explicit}' does not resolve (git rev-parse --verify failed): \
                 {stderr}"
            )));
        }
        Some(explicit.to_owned())
    } else {
        // Probe each candidate base ref. Collect the first one that resolves.
        let candidates: [&str; 2] = ["main", "origin/main"];
        let mut found: Option<&str> = None;
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
                found = Some(candidate);
                break;
            }
        }

        let Some(base_ref) = found else {
            // Neither main nor origin/main exists. This is legitimate only
            // when HEAD itself has no parent (initial commit / orphan
            // branch). Probe for a parent commit; if there is one, the
            // branch just has an unusual base-ref name, which is a hard
            // error rather than vacuous-pass.
            let parent = std::process::Command::new("git")
                .args(["rev-parse", "--verify", "HEAD~1"])
                .output()
                .map_err(|e| {
                    error::LegionError::WorkSource(format!("failed to probe HEAD~1: {e}"))
                })?;
            if parent.status.success() {
                // HEAD has a parent but no known base ref -- refuse rather
                // than vacuously pass. An agent running on an unusual
                // default branch (master, develop, etc.) must configure the
                // base ref (or pass --base explicitly).
                return Err(error::LegionError::WorkSource(
                    "could not find base ref 'main' or 'origin/main' and HEAD has a parent \
                     commit; the simplify gate cannot determine the changed-file set. \
                     Ensure 'main' or 'origin/main' exists in this repo, or pass --base."
                        .to_owned(),
                ));
            }
            // HEAD has no parent: genuine initial commit, vacuously valid.
            // This is unreachable in the normal branch-off-main workflow
            // (HEAD always has a parent once `main` exists), but note it on
            // stderr rather than passing silently -- a vacuous pass should
            // never be invisible to whoever is watching the gate run.
            eprintln!(
                "[legion] no base ref ('main' or 'origin/main') found and HEAD has no parent \
                 commit -- treating this as the initial commit. The changed-file set is empty \
                 and any articulation is accepted vacuously."
            );
            return Ok(ChangedFiles {
                files: std::collections::HashSet::new(),
                cleared_renames: Vec::new(),
                base: None,
            });
        };
        Some(base_ref.to_owned())
    };

    // resolved_base is always Some at this point -- both branches above
    // either set it or return early.
    let base_ref = resolved_base.expect("resolved_base set on every non-early-return path");

    // Run the three-dot diff with core.quotePath=false so non-ASCII paths
    // are returned as-is (no octal escaping) and match heading paths exactly.
    // --name-status -M<pinned similarity> reports renames as their own
    // `R<nn>\t<old>\t<new>` lines instead of collapsing them to the new path
    // alone, which is what lets the parse below tell a pure move (R100) apart
    // from a rename that also touched content (R<100).
    let refspec = format!("{base_ref}...HEAD");
    let out = std::process::Command::new("git")
        .args([
            "-c",
            "core.quotePath=false",
            "diff",
            "--name-status",
            RENAME_SIMILARITY,
            &refspec,
        ])
        .output()
        .map_err(|e| error::LegionError::WorkSource(format!("failed to run git diff: {e}")))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        return Err(error::LegionError::WorkSource(format!(
            "git diff --name-status {refspec} failed (base ref '{base_ref}' resolved \
             but diff did not): {stderr}"
        )));
    }

    let mut files: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut cleared_renames: Vec<(String, String)> = Vec::new();
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let mut fields = line.split('\t');
        let status = fields.next().unwrap_or("");
        if let Some(similarity) = status.strip_prefix('R') {
            // Rename line: `R<nn>\t<old_path>\t<new_path>`.
            let old_path = fields.next();
            let new_path = fields.next();
            match (old_path, new_path) {
                (Some(old_path), Some(new_path)) => {
                    if similarity == "100" {
                        // Zero-delta move: excluded from the coverage set,
                        // recorded for the audit trail.
                        cleared_renames.push((old_path.to_owned(), new_path.to_owned()));
                    } else {
                        // Content delta: the new path carries the diff that
                        // needs review. (The old path no longer exists in
                        // the tree -- there is nothing further to review
                        // "at" it once the rename is detected as such.)
                        files.insert(new_path.to_owned());
                    }
                }
                _ => {
                    return Err(error::LegionError::WorkSource(format!(
                        "git diff --name-status produced a malformed rename line \
                         (expected 'R<nn>\\t<old>\\t<new>'): {line:?}"
                    )));
                }
            }
        } else {
            // Non-rename line (A/M/D/T/...): `<status>\t<path>`. An
            // undetected rename below RENAME_SIMILARITY surfaces here as a
            // separate D line and A line -- both paths land in the coverage
            // set, exactly like any other unrelated delete + add pair.
            if let Some(path) = fields.next() {
                files.insert(path.to_owned());
            }
        }
    }

    Ok(ChangedFiles {
        files,
        cleared_renames,
        base: Some(base_ref),
    })
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
