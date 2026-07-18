//! `legion push` (#791): the sanctioned push path for agents, retiring raw
//! `git push` from agent doctrine. `--delete` (#799) extends the same
//! command with a sanctioned, audited remote-branch-deletion path -- it is a
//! mode of this command, not a separate one.
//!
//! Push mode resolves the checkout that has the target branch checked out
//! via `git worktree list --porcelain` and runs the push FROM that checkout
//! -- the push-from-own-checkout doctrine is enforced by the tool rather
//! than left to agent discipline. The doctrine exists because the pre-push
//! hook reviews the CWD's checked-out branch, not the ref actually being
//! pushed (019f20eb): pushing branch B from a checkout sitting on branch A
//! silently reviews (or blocks on) A's diff instead of B's. Delete mode has
//! no such hook to satisfy (there is no diff to review for a ref deletion),
//! so it runs git commands in the ambient CWD instead of resolving a
//! specific checkout -- deliberate, since the whole point of `--delete` is
//! covering branches that may have no local worktree checkout left at all.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::cli::util::{audit, git_head_commit_and_branch, open_db};
use crate::{db, error};

/// Branches this command refuses to push under any circumstances. Merges to
/// these happen through a reviewed PR, never a direct agent push.
const REFUSED_BRANCHES: [&str; 2] = ["main", "master"];

/// One `git worktree list --porcelain` entry: the checkout path, its HEAD
/// commit, and the branch it has checked out (`None` for a detached HEAD or
/// a bare entry).
#[derive(Debug, Clone, PartialEq, Eq)]
struct WorktreeEntry {
    path: PathBuf,
    head_sha: Option<String>,
    branch: Option<String>,
}

pub(crate) fn handle_push(
    repo: String,
    branch: Option<String>,
    delete: bool,
    force_unmerged: bool,
) -> error::Result<()> {
    if delete {
        return handle_push_delete(repo, branch, force_unmerged);
    }
    if force_unmerged {
        // Clap has no built-in "requires" enforcement across two plain bool
        // flags without a group; enforced here instead so a stray
        // `--force-unmerged` on a plain push fails loudly rather than
        // silently doing nothing.
        return Err(error::LegionError::WorkSource(
            "--force-unmerged only applies together with --delete".to_string(),
        ));
    }

    let target_branch = match branch {
        Some(b) => b,
        None => {
            let (_, cwd_branch) = git_head_commit_and_branch()?;
            cwd_branch
        }
    };

    validate_branch(&target_branch)?;

    let entries = list_worktrees()?;
    let entry = resolve_checkout(&entries, &target_branch)?;
    let checkout_path = entry.path.clone();
    let head_sha = entry.head_sha.clone();

    // Opened before the push (not after) so a DB-open failure fails fast
    // rather than masking the actual push result behind a DB error once the
    // push has already happened.
    let database = open_db()?;

    info!(
        "[legion] pushing '{target_branch}' from {}",
        checkout_path.display()
    );

    let push_result = run_push(&checkout_path, &target_branch);

    // Audit every attempt, success or failure -- the audit trail is the
    // point of routing pushes through this command instead of raw `git
    // push`, so a hook-blocked push must leave a row just as a successful
    // one does. The error (if any) propagates AFTER the row is written.
    let details = serde_json::json!({
        "checkout": checkout_path.display().to_string(),
        "head_sha": head_sha,
    })
    .to_string();
    audit(
        &database,
        &db::AuditInput {
            agent: &repo,
            action: "push",
            target_type: "branch",
            target_ref: &target_branch,
            task_id: None,
            source_type: "git",
            details: Some(&details),
            outcome: if push_result.is_ok() {
                "success"
            } else {
                "failure"
            },
        },
    );

    push_result?;

    println!(
        "pushed {target_branch} to origin ({})",
        checkout_path.display()
    );
    Ok(())
}

/// Reject anything that is not a plain branch name: empty, a leading `-`
/// (could be parsed as a git flag, e.g. a `--branch '--force'` smuggle
/// attempt), a leading `+` (git's force-push refspec marker), an embedded
/// `:` (refspec source:dest separator -- could retarget the push to an
/// unrelated remote ref), or embedded whitespace. Neither push mode nor
/// delete mode has a `--force` flag; this guard closes the gap where a
/// crafted `--branch` value could recover force/retarget semantics anyway.
/// Shared by [`validate_branch`] (push) and [`validate_delete_branch`]
/// (delete), which each layer their own main/master refusal on top with a
/// mode-specific error variant.
fn validate_branch_shape(branch: &str) -> error::Result<()> {
    if branch.is_empty()
        || branch.starts_with('-')
        || branch.starts_with('+')
        || branch.contains(':')
        || branch.chars().any(char::is_whitespace)
    {
        return Err(error::LegionError::PushRefused {
            branch: branch.to_string(),
            reason: "not a plain branch name (must not start with '-'/'+', contain ':', or \
                      contain whitespace) -- no flag exists on this command to force-push or \
                      retarget the ref"
                .to_string(),
        });
    }
    Ok(())
}

/// Full validation for push mode: shape safety plus the main/master
/// refusal.
fn validate_branch(branch: &str) -> error::Result<()> {
    validate_branch_shape(branch)?;
    if REFUSED_BRANCHES.contains(&branch) {
        return Err(error::LegionError::PushRefused {
            branch: branch.to_string(),
            reason: "agents never push main/master directly -- merges happen through a \
                      reviewed PR"
                .to_string(),
        });
    }
    Ok(())
}

/// Full validation for delete mode: shape safety plus the main/master
/// refusal, using [`error::LegionError::PushDeleteRefusedProtectedRef`] (a
/// distinct variant from push's refusal, per #799 -- no override exists for
/// either, but delete's audit/error surface names the delete-specific
/// reason).
fn validate_delete_branch(branch: &str) -> error::Result<()> {
    validate_branch_shape(branch)?;
    if REFUSED_BRANCHES.contains(&branch) {
        return Err(error::LegionError::PushDeleteRefusedProtectedRef {
            branch: branch.to_string(),
        });
    }
    Ok(())
}

/// Find the worktree entry with `branch` checked out. Errors naming every
/// searched checkout path when none match.
fn resolve_checkout<'a>(
    entries: &'a [WorktreeEntry],
    branch: &str,
) -> error::Result<&'a WorktreeEntry> {
    entries
        .iter()
        .find(|e| e.branch.as_deref() == Some(branch))
        .ok_or_else(|| {
            let searched = entries
                .iter()
                .map(|e| e.path.display().to_string())
                .collect::<Vec<_>>()
                .join(", ");
            error::LegionError::PushBranchNotFound {
                branch: branch.to_string(),
                searched,
            }
        })
}

/// Run `git worktree list --porcelain` (ambient CWD -- lists every worktree
/// of whichever repo the caller is standing in, regardless of which linked
/// checkout that happens to be) and parse the result.
fn list_worktrees() -> error::Result<Vec<WorktreeEntry>> {
    let output = Command::new("git")
        .args(["worktree", "list", "--porcelain"])
        .output()
        .map_err(|e| {
            error::LegionError::WorkSource(format!("failed to run git worktree list: {e}"))
        })?;
    if !output.status.success() {
        return Err(error::LegionError::WorkSource(format!(
            "git worktree list --porcelain failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    Ok(parse_worktree_list_porcelain(&String::from_utf8_lossy(
        &output.stdout,
    )))
}

/// Pure parser for `git worktree list --porcelain` output, isolated from
/// the git invocation so it is unit-testable without a real repo. Entries
/// are separated by a blank line; each carries a `worktree <path>` line, an
/// optional `HEAD <sha>` line, and either `branch refs/heads/<name>`,
/// `bare`, or `detached`.
fn parse_worktree_list_porcelain(text: &str) -> Vec<WorktreeEntry> {
    let mut entries = Vec::new();
    let mut path: Option<PathBuf> = None;
    let mut head_sha: Option<String> = None;
    let mut branch: Option<String> = None;

    let flush = |path: &mut Option<PathBuf>,
                 head_sha: &mut Option<String>,
                 branch: &mut Option<String>,
                 entries: &mut Vec<WorktreeEntry>| {
        if let Some(p) = path.take() {
            entries.push(WorktreeEntry {
                path: p,
                head_sha: head_sha.take(),
                branch: branch.take(),
            });
        }
    };

    for line in text.lines() {
        if line.is_empty() {
            flush(&mut path, &mut head_sha, &mut branch, &mut entries);
            continue;
        }
        if let Some(p) = line.strip_prefix("worktree ") {
            path = Some(PathBuf::from(p));
        } else if let Some(sha) = line.strip_prefix("HEAD ") {
            head_sha = Some(sha.to_string());
        } else if let Some(b) = line.strip_prefix("branch ") {
            branch = Some(b.trim_start_matches("refs/heads/").to_string());
        }
        // "bare" / "detached" / "locked ..." / "prunable ..." lines carry no
        // field this command needs.
    }
    // Porcelain output does not reliably end with a trailing blank line --
    // flush whatever block is still open.
    flush(&mut path, &mut head_sha, &mut branch, &mut entries);

    entries
}

/// Run `git -C <checkout> push -u origin <branch>`. `-u` runs on every push,
/// not just the first -- it is a no-op once the upstream is already set, so
/// this avoids an extra `@{upstream}` probe to distinguish first-push from
/// steady-state. stderr is relayed live, line by line, AND captured for the
/// failure message: a long-running hook (the nested-claude pre-push review)
/// must be visible as it happens, not only after the whole push completes.
fn run_push(checkout: &Path, branch: &str) -> error::Result<()> {
    let mut child = Command::new("git")
        .arg("-C")
        .arg(checkout)
        .args(["push", "-u", "origin", branch])
        .stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| error::LegionError::WorkSource(format!("failed to spawn git push: {e}")))?;

    let stderr_pipe = child
        .stderr
        .take()
        .ok_or_else(|| error::LegionError::WorkSource("git push stderr missing".to_string()))?;
    let captured = relay_and_capture_stderr(stderr_pipe);

    let status = child
        .wait()
        .map_err(|e| error::LegionError::WorkSource(format!("git push wait failed: {e}")))?;

    if !status.success() {
        return Err(error::LegionError::PushFailed { stderr: captured });
    }

    Ok(())
}

/// Read `pipe` line by line, relaying each line to our own stderr as it
/// arrives and accumulating it into the returned string.
fn relay_and_capture_stderr(pipe: impl std::io::Read) -> String {
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

// ---------------------------------------------------------------------------
// Delete mode (#799).
// ---------------------------------------------------------------------------

/// Outcome of a successful delete attempt, carried out of [`attempt_delete`]
/// into the audit details. `merged_into_sha` is `Some` only when ancestry
/// into the default branch was actually verified before deletion,
/// regardless of whether `--force-unmerged` was also passed -- so the audit
/// trail never claims a merge that did not happen, even on a run that used
/// the override.
struct DeleteAttempt {
    merged_into_sha: Option<String>,
    default_branch_sha: String,
}

/// `legion push --delete`: delete `target_branch` from `origin`, refusing
/// `main`/`master` unconditionally and refusing anything not fully merged
/// into the remote default branch unless `force_unmerged` is set. On a
/// successful remote delete, best-effort prunes the local branch and any
/// worktree checkout of it (only if clean).
///
/// Mirrors [`handle_push`]'s audit discipline: every attempt past shape/
/// protected-ref validation is logged, success or failure, before the
/// result propagates.
fn handle_push_delete(
    repo: String,
    branch: Option<String>,
    force_unmerged: bool,
) -> error::Result<()> {
    let target_branch = match branch {
        Some(b) => b,
        None => {
            let (_, cwd_branch) = git_head_commit_and_branch()?;
            cwd_branch
        }
    };

    // Shape/protected-ref refusal fails fast, unaudited -- same convention
    // as push mode's validate_branch: this is a hard input gate with no
    // override, not an attempt worth a trail entry.
    validate_delete_branch(&target_branch)?;

    let entries = list_worktrees()?;

    // Opened before the delete attempt for the same reason push mode opens
    // it early: a DB-open failure must fail fast, not mask the real result
    // behind a DB error after the remote delete already happened.
    let database = open_db()?;

    let action = if force_unmerged {
        "push-delete-force-unmerged"
    } else {
        "push-delete"
    };

    info!("[legion] deleting '{target_branch}' from origin");

    let attempt = attempt_delete(&target_branch, force_unmerged);

    let (merged_into, default_branch_sha) = match &attempt {
        Ok(a) => (
            a.merged_into_sha.clone(),
            Some(a.default_branch_sha.clone()),
        ),
        Err(_) => (None, None),
    };
    let details = serde_json::json!({
        "merged_into": merged_into,
        "default_branch_sha": default_branch_sha,
        "force_unmerged": force_unmerged,
    })
    .to_string();
    audit(
        &database,
        &db::AuditInput {
            agent: &repo,
            action,
            target_type: "branch",
            target_ref: &target_branch,
            task_id: None,
            source_type: "git",
            details: Some(&details),
            outcome: if attempt.is_ok() {
                "success"
            } else {
                "failure"
            },
        },
    );

    attempt?;

    let cleanup_summary = prune_local(&target_branch, &entries);
    println!("deleted {target_branch} from origin ({cleanup_summary})");
    Ok(())
}

/// Fetch, check merge status against the fresh remote default branch, and
/// (if merged, or overridden) run the actual `git push origin --delete`.
/// Returns `Err` on any failure in the chain: fetch failure, an
/// undeterminable default branch, the unmerged refusal, or the underlying
/// delete itself failing.
fn attempt_delete(branch: &str, force_unmerged: bool) -> error::Result<DeleteAttempt> {
    fetch_origin()?;

    let (default_ref, default_sha) = resolve_default_remote_ref()?;
    let branch_remote_ref = format!("refs/remotes/origin/{branch}");
    let branch_sha = rev_parse(&branch_remote_ref)?;

    let mut merged = false;
    if let Some(branch_sha_val) = &branch_sha {
        merged = is_ancestor(branch_sha_val, &default_sha)?;
        if !merged && !force_unmerged {
            let tips = unmerged_tip_commits(&default_ref, &branch_remote_ref)?;
            return Err(error::LegionError::PushDeleteRefusedUnmerged {
                branch: branch.to_string(),
                tips,
            });
        }
    }
    // branch_sha is None: the branch is not on the remote at all (already
    // deleted, or never pushed). Nothing to merge-check -- fall through so
    // the delete attempt below surfaces git's own "remote ref does not
    // exist" failure rather than this command fabricating one.

    run_push_delete(branch)?;

    Ok(DeleteAttempt {
        merged_into_sha: merged.then_some(default_sha.clone()),
        default_branch_sha: default_sha,
    })
}

/// `git fetch origin`, quiet. Delete mode's merged-check must never trust a
/// possibly-stale local view of the remote (#799) -- this refreshes every
/// remote-tracking ref before any ancestry check runs.
fn fetch_origin() -> error::Result<()> {
    let out = Command::new("git")
        .args(["fetch", "origin", "-q"])
        .output()
        .map_err(|e| error::LegionError::WorkSource(format!("failed to run git fetch: {e}")))?;
    if !out.status.success() {
        return Err(error::LegionError::WorkSource(format!(
            "git fetch origin failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    Ok(())
}

/// `git rev-parse --verify <refname>`. `Ok(None)` (not `Err`) when the ref
/// does not resolve -- a missing ref is a normal, expected outcome for
/// several callers here (branch already gone from the remote, no local
/// branch left after a worktree prune), not a plumbing failure.
fn rev_parse(refname: &str) -> error::Result<Option<String>> {
    let out = Command::new("git")
        .args(["rev-parse", "--verify", refname])
        .output()
        .map_err(|e| {
            error::LegionError::WorkSource(format!("failed to run git rev-parse {refname}: {e}"))
        })?;
    if out.status.success() {
        Ok(Some(
            String::from_utf8_lossy(&out.stdout).trim().to_string(),
        ))
    } else {
        Ok(None)
    }
}

/// Resolve the remote-tracking ref for the repo's default branch, trying
/// `origin/main` then `origin/master` (mirrors [`REFUSED_BRANCHES`]). Must
/// run after [`fetch_origin`] so the ref reflects current remote state.
/// Returns the ref name and its resolved SHA together so callers never
/// re-query for the SHA a second time.
fn resolve_default_remote_ref() -> error::Result<(String, String)> {
    for candidate in ["refs/remotes/origin/main", "refs/remotes/origin/master"] {
        if let Some(sha) = rev_parse(candidate)? {
            return Ok((candidate.to_string(), sha));
        }
    }
    Err(error::LegionError::WorkSource(
        "neither origin/main nor origin/master resolved after fetch -- cannot determine the \
         default branch for the merged-check"
            .to_string(),
    ))
}

/// `git merge-base --is-ancestor <ancestor> <descendant>`. Exit 0 means
/// `ancestor` is reachable from `descendant` (merged); exit 1 means it is
/// not. Any other exit code is a plumbing failure (e.g. a missing object),
/// surfaced as an error rather than silently read as "not merged".
fn is_ancestor(ancestor: &str, descendant: &str) -> error::Result<bool> {
    let out = Command::new("git")
        .args(["merge-base", "--is-ancestor", ancestor, descendant])
        .output()
        .map_err(|e| {
            error::LegionError::WorkSource(format!(
                "failed to run git merge-base --is-ancestor: {e}"
            ))
        })?;
    match out.status.code() {
        Some(0) => Ok(true),
        Some(1) => Ok(false),
        _ => Err(error::LegionError::WorkSource(format!(
            "git merge-base --is-ancestor exited unexpectedly ({:?}): {}",
            out.status.code(),
            String::from_utf8_lossy(&out.stderr).trim()
        ))),
    }
}

/// The commits reachable from `branch_ref` but not `default_ref`
/// (`git rev-list --abbrev-commit <default_ref>..<branch_ref>`), joined and
/// char-bounded (never a naive byte slice -- unsafe on multi-byte UTF-8) via
/// the crate-wide `card_parse::truncate_chars` so an unmerged branch with a
/// long history cannot blow up the refusal error message.
fn unmerged_tip_commits(default_ref: &str, branch_ref: &str) -> error::Result<String> {
    let range = format!("{default_ref}..{branch_ref}");
    let out = Command::new("git")
        .args(["rev-list", "--abbrev-commit", &range])
        .output()
        .map_err(|e| {
            error::LegionError::WorkSource(format!("failed to run git rev-list {range}: {e}"))
        })?;
    if !out.status.success() {
        return Err(error::LegionError::WorkSource(format!(
            "git rev-list {range} failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    let joined = String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join(", ");
    Ok(crate::card_parse::truncate_chars(&joined, 200))
}

/// `git push origin --delete <branch>` in the ambient CWD. Mirrors
/// [`run_push`]'s live stderr relay + capture, but has no checkout to
/// resolve first -- see the module doc comment for why delete mode runs
/// from CWD rather than a resolved worktree.
fn run_push_delete(branch: &str) -> error::Result<()> {
    let mut child = Command::new("git")
        .args(["push", "origin", "--delete", branch])
        .stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| {
            error::LegionError::WorkSource(format!("failed to spawn git push --delete: {e}"))
        })?;

    let stderr_pipe = child.stderr.take().ok_or_else(|| {
        error::LegionError::WorkSource("git push --delete stderr missing".to_string())
    })?;
    let captured = relay_and_capture_stderr(stderr_pipe);

    let status = child.wait().map_err(|e| {
        error::LegionError::WorkSource(format!("git push --delete wait failed: {e}"))
    })?;

    if !status.success() {
        return Err(error::LegionError::PushDeleteRemoteFailure { stderr: captured });
    }

    Ok(())
}

/// Best-effort local cleanup after a successful remote delete: if a
/// worktree has `branch` checked out and is clean, remove the worktree and
/// delete the local branch; if a local branch exists with no worktree
/// checkout, delete it directly; otherwise there is nothing to prune. Never
/// force-removes a dirty worktree or force-deletes a branch (`git branch
/// -d`, not `-D`) -- failures here are reported, not escalated, since the
/// remote delete (the audited, sanctioned action) already succeeded.
fn prune_local(branch: &str, entries: &[WorktreeEntry]) -> String {
    if let Some(entry) = entries.iter().find(|e| e.branch.as_deref() == Some(branch)) {
        let status = Command::new("git")
            .arg("-C")
            .arg(&entry.path)
            .args(["status", "--porcelain"])
            .output();
        return match status {
            Ok(out) if out.status.success() && out.stdout.is_empty() => {
                let rm = Command::new("git")
                    .args(["worktree", "remove", &entry.path.display().to_string()])
                    .output();
                match rm {
                    Ok(o) if o.status.success() => describe_local_branch_delete(
                        delete_local_branch(branch),
                        "worktree removed and local branch deleted",
                        "worktree removed; local branch delete failed (left behind)",
                        "worktree removed; no local branch ref remained",
                    ),
                    Ok(o) => format!(
                        "left behind: git worktree remove failed for {}: {}",
                        entry.path.display(),
                        String::from_utf8_lossy(&o.stderr).trim()
                    ),
                    Err(e) => format!(
                        "left behind: could not run git worktree remove for {}: {e}",
                        entry.path.display()
                    ),
                }
            }
            Ok(_) => format!(
                "left behind: worktree at {} has uncommitted changes",
                entry.path.display()
            ),
            Err(e) => format!(
                "left behind: could not check worktree cleanliness at {}: {e}",
                entry.path.display()
            ),
        };
    }

    describe_local_branch_delete(
        delete_local_branch(branch),
        "no worktree checkout found; local branch deleted",
        "no worktree checkout found; local branch exists but delete failed (left behind)",
        "no local branch or worktree checkout found",
    )
}

/// Render a [`delete_local_branch`] result into the human-readable summary
/// `prune_local` returns, sharing the three-way (deleted / failed / absent)
/// wording between the worktree and no-worktree branches instead of
/// duplicating the match arms.
fn describe_local_branch_delete(
    result: error::Result<Option<bool>>,
    deleted: &str,
    failed: &str,
    absent: &str,
) -> String {
    match result {
        Ok(Some(true)) => deleted.to_string(),
        Ok(Some(false)) => failed.to_string(),
        Ok(None) => absent.to_string(),
        Err(_) => absent.to_string(),
    }
}

/// Delete the local branch ref `branch` (`git branch -d`, never `-D`).
/// `Ok(None)` when no local branch ref exists at all; `Ok(Some(true))` on a
/// successful delete; `Ok(Some(false))` when the ref exists but the delete
/// failed (e.g. not merged into the current local HEAD -- deliberately not
/// escalated to `-D`, matching the "only if clean" / no-force spirit of the
/// local prune).
fn delete_local_branch(branch: &str) -> error::Result<Option<bool>> {
    let exists = rev_parse(&format!("refs/heads/{branch}"))?.is_some();
    if !exists {
        return Ok(None);
    }
    let out = Command::new("git")
        .args(["branch", "-d", branch])
        .output()
        .map_err(|e| {
            error::LegionError::WorkSource(format!("failed to run git branch -d {branch}: {e}"))
        })?;
    Ok(Some(out.status.success()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_worktree_list_porcelain_multiple_entries() {
        let text = "worktree /repo/main\n\
                     HEAD abc123\n\
                     branch refs/heads/main\n\
                     \n\
                     worktree /repo/feat\n\
                     HEAD def456\n\
                     branch refs/heads/feat/x\n\
                     \n";
        let entries = parse_worktree_list_porcelain(text);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].path, PathBuf::from("/repo/main"));
        assert_eq!(entries[0].head_sha.as_deref(), Some("abc123"));
        assert_eq!(entries[0].branch.as_deref(), Some("main"));
        assert_eq!(entries[1].path, PathBuf::from("/repo/feat"));
        assert_eq!(entries[1].branch.as_deref(), Some("feat/x"));
    }

    #[test]
    fn parse_worktree_list_porcelain_no_trailing_blank_line() {
        // git does not guarantee a trailing blank line after the last
        // block; the flush-at-end path must still capture it.
        let text = "worktree /repo/main\nHEAD abc123\nbranch refs/heads/main";
        let entries = parse_worktree_list_porcelain(text);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].branch.as_deref(), Some("main"));
    }

    #[test]
    fn parse_worktree_list_porcelain_detached_head_has_no_branch() {
        let text = "worktree /repo/detached\nHEAD abc123\ndetached\n";
        let entries = parse_worktree_list_porcelain(text);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].branch, None);
        assert_eq!(entries[0].head_sha.as_deref(), Some("abc123"));
    }

    #[test]
    fn parse_worktree_list_porcelain_bare_entry() {
        let text = "worktree /repo/bare\nbare\n\n";
        let entries = parse_worktree_list_porcelain(text);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].branch, None);
        assert_eq!(entries[0].head_sha, None);
    }

    #[test]
    fn resolve_checkout_finds_matching_branch() {
        let entries = vec![
            WorktreeEntry {
                path: PathBuf::from("/repo/main"),
                head_sha: Some("abc".to_string()),
                branch: Some("main".to_string()),
            },
            WorktreeEntry {
                path: PathBuf::from("/repo/feat"),
                head_sha: Some("def".to_string()),
                branch: Some("feat/x".to_string()),
            },
        ];
        let found = resolve_checkout(&entries, "feat/x").expect("must find feat/x");
        assert_eq!(found.path, PathBuf::from("/repo/feat"));
    }

    #[test]
    fn resolve_checkout_errors_naming_searched_paths() {
        let entries = vec![
            WorktreeEntry {
                path: PathBuf::from("/repo/main"),
                head_sha: None,
                branch: Some("main".to_string()),
            },
            WorktreeEntry {
                path: PathBuf::from("/repo/other"),
                head_sha: None,
                branch: Some("other".to_string()),
            },
        ];
        let err = resolve_checkout(&entries, "feat/missing").unwrap_err();
        match err {
            error::LegionError::PushBranchNotFound { branch, searched } => {
                assert_eq!(branch, "feat/missing");
                assert!(searched.contains("/repo/main"));
                assert!(searched.contains("/repo/other"));
            }
            other => panic!("expected PushBranchNotFound, got {other:?}"),
        }
    }

    #[test]
    fn validate_branch_refuses_main_and_master() {
        assert!(validate_branch("main").is_err());
        assert!(validate_branch("master").is_err());
        assert!(validate_branch("feat/main-fix").is_ok());
    }

    #[test]
    fn validate_branch_refuses_flag_shaped_values() {
        assert!(validate_branch("--force").is_err());
        assert!(validate_branch("-f").is_err());
    }

    #[test]
    fn validate_branch_refuses_force_prefix() {
        assert!(validate_branch("+feat/x").is_err());
    }

    #[test]
    fn validate_branch_refuses_refspec_separator() {
        assert!(validate_branch("feat/x:refs/heads/other").is_err());
    }

    #[test]
    fn validate_branch_refuses_whitespace_and_empty() {
        assert!(validate_branch("").is_err());
        assert!(validate_branch("feat x").is_err());
    }

    #[test]
    fn validate_branch_accepts_plain_names() {
        assert!(validate_branch("feat/791-legion-push").is_ok());
        assert!(validate_branch("some-branch").is_ok());
    }

    #[test]
    fn validate_delete_branch_refuses_main_and_master_with_distinct_variant() {
        let err_main = validate_delete_branch("main").unwrap_err();
        assert!(matches!(
            err_main,
            error::LegionError::PushDeleteRefusedProtectedRef { branch } if branch == "main"
        ));
        let err_master = validate_delete_branch("master").unwrap_err();
        assert!(matches!(
            err_master,
            error::LegionError::PushDeleteRefusedProtectedRef { branch } if branch == "master"
        ));
    }

    #[test]
    fn validate_delete_branch_accepts_plain_names() {
        assert!(validate_delete_branch("feat/799-push-delete").is_ok());
    }

    #[test]
    fn validate_delete_branch_refuses_flag_shaped_values() {
        // Shape refusal is shared with push mode's validate_branch and
        // still surfaces the generic PushRefused variant, not the
        // protected-ref one -- main/master is the only delete-specific
        // refusal reason.
        let err = validate_delete_branch("--force").unwrap_err();
        assert!(matches!(err, error::LegionError::PushRefused { .. }));
    }

    #[test]
    fn describe_local_branch_delete_maps_all_three_outcomes() {
        assert_eq!(
            describe_local_branch_delete(Ok(Some(true)), "deleted", "failed", "absent"),
            "deleted"
        );
        assert_eq!(
            describe_local_branch_delete(Ok(Some(false)), "deleted", "failed", "absent"),
            "failed"
        );
        assert_eq!(
            describe_local_branch_delete(Ok(None), "deleted", "failed", "absent"),
            "absent"
        );
        assert_eq!(
            describe_local_branch_delete(
                Err(error::LegionError::WorkSource("boom".to_string())),
                "deleted",
                "failed",
                "absent"
            ),
            "absent"
        );
    }
}
