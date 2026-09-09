//! `legion push` (#791): the sanctioned push path for agents, retiring raw
//! `git push` from agent doctrine.
//!
//! Resolves the checkout that has the target branch checked out via `git
//! worktree list --porcelain` and runs the push FROM that checkout -- the
//! push-from-own-checkout doctrine is enforced by the tool rather than left
//! to agent discipline. The doctrine exists because the pre-push hook
//! reviews the CWD's checked-out branch, not the ref actually being pushed
//! (019f20eb): pushing branch B from a checkout sitting on branch A silently
//! reviews (or blocks on) A's diff instead of B's.
//!
//! `--force` (#1172) is a sanctioned, gated force-push, not a raw
//! passthrough -- see [`analyze_force_push`] for the discarded-commit
//! survival analysis that gates it.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::cli::util::{audit, git_head_commit_and_branch, open_db, relay_and_capture_stderr};
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
    tag: Option<String>,
    force: bool,
    force_reason: Option<String>,
) -> error::Result<()> {
    if let Some(t) = tag {
        // clap's `conflicts_with = "tag"` on `force` already refuses this
        // combination before we get here; force/force_reason are inert on
        // the tag path.
        return handle_push_tag(repo, t);
    }
    handle_push_branch(repo, branch, force, force_reason)
}

/// Push a tag (#915).
///
/// Deliberately does NOT resolve a checkout the way the branch path does. A
/// branch belongs to one worktree, and the push-from-own-checkout doctrine
/// exists because the pre-push hook reviews the CWD's checked-out branch. A
/// tag is a repo-wide ref owned by no worktree, so there is no "checkout that
/// has it" to find -- every worktree shares the object store that holds it.
/// We push from the cwd and record which checkout that was.
fn handle_push_tag(repo: String, tag: String) -> error::Result<()> {
    validate_tag(&tag)?;

    let checkout = std::env::current_dir().map_err(|e| {
        error::LegionError::WorkSource(format!("cannot resolve the current directory: {e}"))
    })?;

    // Resolve before pushing so a nonexistent tag is a named refusal rather
    // than a git error surfacing from inside the push.
    let target_sha = resolve_tag_commit(&checkout, &tag)?;

    // Operator ruling (#915): a tag on a commit no branch on origin contains
    // publishes a ref that resolves for the tagger and dangles for everyone
    // else. Refuse before the push, not after.
    if !commit_is_on_origin(&checkout, &target_sha)? {
        return Err(error::LegionError::PushRefused {
            branch: tag.clone(),
            reason: format!(
                "tag '{tag}' points at {target_sha}, which is not reachable from any branch on \
                 origin -- push the branch carrying that commit first, or the tag will dangle \
                 for everyone but you"
            ),
        });
    }

    let database = open_db()?;

    info!("[legion] pushing tag '{tag}' from {}", checkout.display());

    let push_result = run_push_tag(&checkout, &tag);

    // Same contract as the branch path: audit every attempt before
    // propagating the error, so a hook-blocked push still leaves a row.
    let details = serde_json::json!({
        "checkout": checkout.display().to_string(),
        "target_sha": target_sha,
    })
    .to_string();
    audit(
        &database,
        &db::AuditInput {
            agent: &repo,
            action: "push",
            target_type: "tag",
            target_ref: &tag,
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
        "pushed tag {tag} -> {target_sha} to origin ({})",
        checkout.display()
    );
    Ok(())
}

fn handle_push_branch(
    repo: String,
    branch: Option<String>,
    force: bool,
    force_reason: Option<String>,
) -> error::Result<()> {
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

    if force {
        return handle_force_push(
            &database,
            &repo,
            &target_branch,
            &checkout_path,
            head_sha.as_deref(),
            force_reason.as_deref(),
        );
    }

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

/// Which check a discarded commit survived, or that it survived none of
/// them -- the orphan case that triggers a refusal (#1172).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SurvivalVerdict {
    /// Patch-id equivalent to a commit unique to the new local history (the
    /// ordinary amend/rebase case: same change, new sha).
    PresentInNewHistory,
    /// Reachable from `main` under the same sha (an ordinary merge/
    /// fast-forward already landed it).
    AlreadyInMainBySha,
    /// Patch-id equivalent to a commit unique to `main` (the squash-merge
    /// case: the commit's content is on `main` under a different sha) --
    /// covers a squash that combined exactly ONE commit, where the squash
    /// commit's whole diff equals this commit's own diff.
    AlreadyInMainByPatchId,
    /// Not individually patch-id equivalent to anything, but every file the
    /// WHOLE remaining-orphan set touched is byte-identical between the old
    /// remote head and the new local head -- the multi-commit squash case
    /// (patch-id is per-commit, so three pre-squash commits combined into
    /// one main commit never patch-id-match individually, even though their
    /// content is fully upstream). See the cumulative fallback in
    /// [`analyze_force_push`].
    AlreadyInMainByCumulativeDiff,
    /// None of the above -- this push would genuinely destroy the commit.
    Orphan,
}

impl SurvivalVerdict {
    fn survives(self) -> bool {
        !matches!(self, SurvivalVerdict::Orphan)
    }

    fn label(self) -> &'static str {
        match self {
            SurvivalVerdict::PresentInNewHistory => "present-in-new-history",
            SurvivalVerdict::AlreadyInMainBySha => "already-in-main-by-sha",
            SurvivalVerdict::AlreadyInMainByPatchId => "already-in-main-by-patch-id",
            SurvivalVerdict::AlreadyInMainByCumulativeDiff => "already-in-main-by-cumulative-diff",
            SurvivalVerdict::Orphan => "orphan",
        }
    }
}

/// One commit the force-push would discard (reachable from the remote head,
/// not from the new local head), plus which survival test it passed.
#[derive(Debug, Clone, PartialEq, Eq)]
struct DiscardedCommit {
    sha: String,
    subject: String,
    verdict: SurvivalVerdict,
}

/// Result of [`analyze_force_push`]: what the remote head sha was (`None`
/// when the branch has never been pushed) and every commit the push would
/// discard.
struct ForceAnalysis {
    old_remote_sha: Option<String>,
    discarded: Vec<DiscardedCommit>,
}

/// The discarded-commit analysis that gates `--force` (#1172).
///
/// Fetches the remote ref fresh (never trusts a stale local remote-tracking
/// branch), then computes the commits reachable from that remote head but
/// NOT from `new_local_head` -- what the push would discard. Each one is
/// checked against three survival tests, in this order (main first, new
/// local history last -- see the ordering note in the loop below for why):
///
/// 1. Reachable from `main` under the same sha (`git merge-base
///    --is-ancestor`). An ordinary merge/fast-forward already landed it.
/// 2. Patch-id equivalent to a commit unique to `main` (`git cherry
///    <main_ref> <old_remote_sha>`). This is the squash-merge case this
///    issue exists for -- a rebased stacked branch whose base just
///    squash-merged -- but ONLY when the squash combined a single commit
///    (the squash commit's whole diff then equals this commit's own diff).
/// 3. Patch-id equivalent to a commit unique to the new local history (`git
///    cherry <new_local_head> <old_remote_sha>` -- candidates are exactly
///    `new_local_head..old_remote_sha`, checked against
///    `old_remote_sha..new_local_head`; a `-` mark means an equivalent
///    change is already in the new history). This is the ordinary
///    amend/rebase case.
///
/// Anything still failing all three falls to a CUMULATIVE check before
/// being declared an orphan: a squash-merge that combines MULTIPLE commits
/// into one main commit cannot patch-id-match any single pre-squash commit
/// (patch-id is computed per-commit, not cumulatively) even though the
/// content is fully upstream -- this is the actual #1167->#1168 incident
/// #1172 was filed for, not an edge case. For every commit still failing
/// tests 1-3, take the union of files touched across ALL of them and
/// compare their content directly between `old_remote_sha` and
/// `new_local_head`. If every one of those files is byte-identical in the
/// new state, the whole remaining group is upgraded to
/// [`SurvivalVerdict::AlreadyInMainByCumulativeDiff`] together -- nothing
/// was actually lost in aggregate, however many shas it took to get there.
/// A real loss anywhere in that group leaves at least one file differing,
/// so this never upgrades a genuine loss to "survives"; it CAN
/// conservatively decline to upgrade a legitimate squash if an unrelated
/// genuine loss happens to share the same remaining-orphan batch -- that
/// mixed case still requires `--force-reason` for the whole batch, which is
/// the safe direction to fail in.
///
/// A commit that fails all four is a genuine orphan.
fn analyze_force_push(
    checkout: &Path,
    branch: &str,
    new_local_head: &str,
) -> error::Result<ForceAnalysis> {
    let Some(old_remote_sha) = fetch_remote_head(checkout, branch)? else {
        // Branch has never been pushed -- nothing to discard.
        return Ok(ForceAnalysis {
            old_remote_sha: None,
            discarded: Vec::new(),
        });
    };

    let discarded_shas = rev_list_exclusive(checkout, new_local_head, &old_remote_sha)?;
    if discarded_shas.is_empty() {
        return Ok(ForceAnalysis {
            old_remote_sha: Some(old_remote_sha),
            discarded: Vec::new(),
        });
    }

    // Fetched fresh (not read from a possibly-stale local `main`) precisely
    // because the motivating scenario is a stacked branch whose base JUST
    // squash-merged -- a stale local `main` would fail every survival test
    // and turn the case this feature exists for into a refusal.
    let main_ref = fetch_main_ref(checkout)?;

    let cherry_vs_new_local = cherry_marks(checkout, new_local_head, &old_remote_sha)?;
    let cherry_vs_main = cherry_marks(checkout, &main_ref, &old_remote_sha)?;

    let mut discarded = Vec::with_capacity(discarded_shas.len());
    for sha in discarded_shas {
        let subject = commit_subject(checkout, &sha)?;
        // Checked in this order -- main first, new-local-history last --
        // because `new_local_head` is normally built ON TOP OF `main`, so
        // "commits unique to new_local_head" (what `cherry_vs_new_local`
        // checks a candidate against) includes any commit main just gained
        // (e.g. a squash-merge), not only the branch's own replayed work.
        // Checking main first gives the squash-merge case its accurate
        // `AlreadyInMainByPatchId` label instead of the technically-true-
        // but-misleading `PresentInNewHistory` a rebase would otherwise
        // collect it under.
        let verdict = if is_ancestor(checkout, &sha, &main_ref)? {
            SurvivalVerdict::AlreadyInMainBySha
        } else if cherry_vs_main.get(&sha).copied().unwrap_or(false) {
            SurvivalVerdict::AlreadyInMainByPatchId
        } else if cherry_vs_new_local.get(&sha).copied().unwrap_or(false) {
            SurvivalVerdict::PresentInNewHistory
        } else {
            SurvivalVerdict::Orphan
        };
        discarded.push(DiscardedCommit {
            sha,
            subject,
            verdict,
        });
    }

    // Cumulative fallback (see the doc comment above): only spend the extra
    // git calls when at least one commit is still an orphan after the
    // per-commit tests.
    if discarded
        .iter()
        .any(|c| c.verdict == SurvivalVerdict::Orphan)
    {
        let old_base = merge_base(checkout, new_local_head, &old_remote_sha)?;
        let touched_files = diff_name_only(checkout, &old_base, &old_remote_sha, &[])?;
        if !touched_files.is_empty()
            && diff_name_only(checkout, &old_remote_sha, new_local_head, &touched_files)?.is_empty()
        {
            for c in &mut discarded {
                if c.verdict == SurvivalVerdict::Orphan {
                    c.verdict = SurvivalVerdict::AlreadyInMainByCumulativeDiff;
                }
            }
        }
    }

    Ok(ForceAnalysis {
        old_remote_sha: Some(old_remote_sha),
        discarded,
    })
}

/// `legion push --force` (#1172): run the discarded-commit analysis, refuse
/// (audited) if any commit is an orphan and no `--force-reason` was given,
/// otherwise push with `--force-with-lease`. Every attempt -- refused,
/// failed, or succeeded -- is audited with the full analysis, matching the
/// non-force path's "audit every attempt" contract.
fn handle_force_push(
    database: &db::Database,
    repo: &str,
    target_branch: &str,
    checkout_path: &Path,
    head_sha: Option<&str>,
    force_reason: Option<&str>,
) -> error::Result<()> {
    let new_local_head = head_sha.ok_or_else(|| {
        error::LegionError::WorkSource(format!(
            "cannot resolve HEAD sha for the checkout at {} (git worktree list reported no \
             HEAD for '{target_branch}')",
            checkout_path.display()
        ))
    })?;

    info!(
        "[legion] force-push analysis for '{target_branch}' from {}",
        checkout_path.display()
    );

    let analysis = analyze_force_push(checkout_path, target_branch, new_local_head)?;
    let orphans: Vec<&DiscardedCommit> = analysis
        .discarded
        .iter()
        .filter(|c| !c.verdict.survives())
        .collect();

    let details = force_audit_details(&analysis, new_local_head, force_reason);

    if !orphans.is_empty() && force_reason.is_none() {
        audit(
            database,
            &db::AuditInput {
                agent: repo,
                action: "push",
                target_type: "branch",
                target_ref: target_branch,
                task_id: None,
                source_type: "git",
                details: Some(&details),
                outcome: "failure",
            },
        );
        return Err(orphan_refusal(target_branch, &orphans));
    }

    let push_result = match &analysis.old_remote_sha {
        Some(old_remote_sha) => run_push_force(checkout_path, target_branch, old_remote_sha),
        // Never pushed before -- nothing to lease against; an ordinary push
        // is exactly correct (and exercises the same code path/audit shape
        // as the non-force command otherwise would).
        None => run_push(checkout_path, target_branch),
    };

    audit(
        database,
        &db::AuditInput {
            agent: repo,
            action: "push",
            target_type: "branch",
            target_ref: target_branch,
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
        "force-pushed {target_branch} -> {new_local_head} to origin ({}) -- {} discarded \
         commit(s) verified survivable{}",
        checkout_path.display(),
        analysis.discarded.len(),
        match force_reason {
            Some(reason) => format!(", override reason: {reason}"),
            None => String::new(),
        }
    );
    Ok(())
}

/// Audit `details` JSON for a force-push attempt (#1172): old remote sha,
/// new sha, every discarded commit with its survival verdict, and the
/// `--force-reason` when given. Built once and reused for the refusal,
/// failure, and success outcomes so the same evidence lands on the audit
/// row regardless of how the attempt ended -- a force-push (or a refused
/// one) that leaves no record of what it discarded is exactly what this
/// analysis exists to prevent.
fn force_audit_details(
    analysis: &ForceAnalysis,
    new_local_head: &str,
    force_reason: Option<&str>,
) -> String {
    let discarded: Vec<serde_json::Value> = analysis
        .discarded
        .iter()
        .map(|c| {
            serde_json::json!({
                "sha": c.sha,
                "subject": c.subject,
                "survival": c.verdict.label(),
            })
        })
        .collect();
    serde_json::json!({
        "forced": true,
        "old_remote_sha": analysis.old_remote_sha,
        "new_sha": new_local_head,
        "discarded_count": analysis.discarded.len(),
        "discarded": discarded,
        "force_reason": force_reason,
    })
    .to_string()
}

/// Build the refusal error for a force-push with at least one orphaned
/// commit: names each one as `<sha> <subject>` and names `--force-reason` as
/// the override, per the error-handling requirements.
fn orphan_refusal(branch: &str, orphans: &[&DiscardedCommit]) -> error::LegionError {
    let listing = orphans
        .iter()
        .map(|c| format!("{} {}", c.sha, c.subject))
        .collect::<Vec<_>>()
        .join("\n  ");
    error::LegionError::PushRefused {
        branch: branch.to_string(),
        reason: format!(
            "force-push would discard {} commit(s) with no surviving copy -- not present in the \
             new history, not on main by sha, not on main by patch-id, and the combined content \
             these commits touched is not byte-identical in the new state either (the \
             cumulative squash check already ran and did not clear them):\n  {listing}\n\nif you \
             have verified by hand that these are genuinely already upstream (e.g. a squash \
             mixed with other changes since), override with --force-reason \"...\"",
            orphans.len()
        ),
    }
}

/// Reject anything that is not a plain branch name: empty, a leading `-`
/// (could be parsed as a git flag, e.g. a `--branch '--force'` smuggle
/// attempt), a leading `+` (git's force-push refspec marker), an embedded
/// `:` (refspec source:dest separator -- could retarget the push to an
/// unrelated remote ref), or embedded whitespace. This command has no
/// `--force` flag by construction (#791); this guard closes the gap where a
/// crafted `--branch` value could recover force/retarget semantics anyway.
/// Also refuses `main`/`master` outright -- agents never push those
/// directly.
fn validate_branch(branch: &str) -> error::Result<()> {
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

/// Same crafted-shape refusals as [`validate_branch`], for the same reason:
/// this command has no force path, and a crafted value must not be able to
/// recover one. No main/master check -- that rule is about branches, and a
/// tag named `main` is a bad idea but not the hazard this guards.
fn validate_tag(tag: &str) -> error::Result<()> {
    if tag.is_empty()
        || tag.starts_with('-')
        || tag.starts_with('+')
        || tag.contains(':')
        || tag.chars().any(char::is_whitespace)
    {
        return Err(error::LegionError::PushRefused {
            branch: tag.to_string(),
            reason: "not a plain tag name (must not start with '-'/'+', contain ':', or contain \
                      whitespace) -- no flag exists on this command to force-push or retarget \
                      the ref"
                .to_string(),
        });
    }
    Ok(())
}

/// The commit a tag points at, peeled through an annotated tag object.
/// Errors naming the tag when it does not resolve, so a typo is a refusal
/// rather than a git error surfacing from inside the push.
fn resolve_tag_commit(checkout: &Path, tag: &str) -> error::Result<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(checkout)
        .args([
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("{tag}^{{commit}}"),
        ])
        .output()
        .map_err(|e| {
            error::LegionError::WorkSource(format!("failed to spawn git rev-parse: {e}"))
        })?;

    let sha = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if !out.status.success() || sha.is_empty() {
        return Err(error::LegionError::PushRefused {
            branch: tag.to_string(),
            reason: format!("tag '{tag}' does not resolve to a commit in this repository"),
        });
    }
    Ok(sha)
}

/// Whether any remote-tracking branch contains `sha`.
///
/// `git branch -r --contains` is the check rather than comparing against a
/// single branch, because the tagged commit may live on any pushed branch --
/// a release tag on a merged release branch is the normal case.
fn commit_is_on_origin(checkout: &Path, sha: &str) -> error::Result<bool> {
    let out = Command::new("git")
        .arg("-C")
        .arg(checkout)
        .args(["branch", "-r", "--contains", sha])
        .output()
        .map_err(|e| error::LegionError::WorkSource(format!("failed to spawn git branch: {e}")))?;

    // A non-zero exit here means the sha is unknown to the ref graph, which
    // the caller treats the same as "not on origin" -- both are reasons to
    // refuse, and neither should be reported as a git failure.
    if !out.status.success() {
        return Ok(false);
    }
    Ok(!String::from_utf8_lossy(&out.stdout).trim().is_empty())
}

/// Run `git -C <checkout> push origin refs/tags/<tag>`.
///
/// The fully-qualified refspec is deliberate: `push origin <name>` is
/// ambiguous when a branch and a tag share a name, and git's disambiguation
/// is not something to rely on when the whole point is knowing what was
/// pushed. No `-u` -- a tag has no upstream to set.
fn run_push_tag(checkout: &Path, tag: &str) -> error::Result<()> {
    let refspec = format!("refs/tags/{tag}");
    let mut child = Command::new("git")
        .arg("-C")
        .arg(checkout)
        .args(["push", "origin", &refspec])
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

/// Run `git -C <checkout> push --force-with-lease=<branch>:<expected_sha> -u
/// origin <branch>` (#1172). The fully-qualified lease value is deliberate
/// (not a bare `--force-with-lease`): it pins the exact sha this push was
/// analyzed against, so a concurrent push by another agent or node between
/// our fetch and this push loses the race with a named rejection rather than
/// either silently clobbering the concurrent push or silently succeeding
/// against a state we never actually analyzed.
///
/// On a lease rejection, re-fetches to report the sha the remote actually
/// moved to ([`error::LegionError::PushLeaseStale`]) instead of surfacing
/// git's raw "stale info" text. Any other push failure is
/// [`error::LegionError::PushFailed`], same as the non-force path.
fn run_push_force(checkout: &Path, branch: &str, expected_sha: &str) -> error::Result<()> {
    let lease = format!("--force-with-lease={branch}:{expected_sha}");
    let mut child = Command::new("git")
        .arg("-C")
        .arg(checkout)
        // `--force-with-lease=<ref>:<expected>` alone, deliberately NOT also
        // passing bare `--force`: empirically verified against a real
        // stale-lease fixture (the `run_push_force_reports_stale_lease_...`
        // test below) that adding `--force` alongside the fully-qualified
        // lease DISABLES the lease check entirely -- git treats the blanket
        // `--force` as authorizing the non-fast-forward update regardless
        // of whether the lease's expected value matches, defeating the
        // whole point of leasing. The lease flag alone both performs the
        // non-fast-forward update AND enforces the expected-value check.
        .args(["push", "-u", "origin", branch, &lease])
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
        // "stale info" is git's specific marker for a `--force-with-lease`
        // rejection. Deliberately NOT matching the broader `[rejected]` --
        // that marker also covers a plain non-fast-forward, a pre-push hook
        // rejection, or a protected-branch refusal, none of which are
        // "someone pushed concurrently". Misreporting one of those as a
        // stale lease (and re-fetching to report an "actual" sha) would
        // actively mislead the operator away from the real cause, which is
        // in git's own stderr on the `PushFailed` path below.
        if captured.to_lowercase().contains("stale info") {
            let actual = fetch_remote_head(checkout, branch)?
                .unwrap_or_else(|| "unknown (remote ref no longer resolves)".to_string());
            return Err(error::LegionError::PushLeaseStale {
                branch: branch.to_string(),
                expected: expected_sha.to_string(),
                actual,
            });
        }
        return Err(error::LegionError::PushFailed { stderr: captured });
    }

    Ok(())
}

/// Fetch `origin/<branch>` fresh and return its sha, or `None` when the
/// branch does not exist on the remote (first push -- nothing to lease
/// against, nothing that can be discarded). Uses `FETCH_HEAD` rather than a
/// local remote-tracking ref so the result reflects this exact fetch, not
/// whatever the local `refs/remotes/origin/<branch>` happened to hold before
/// it (#1172 -- the whole point is never trusting a stale local view of the
/// remote).
fn fetch_remote_head(checkout: &Path, branch: &str) -> error::Result<Option<String>> {
    let fetch = Command::new("git")
        .arg("-C")
        .arg(checkout)
        .args(["fetch", "origin", branch])
        .output()
        .map_err(|e| error::LegionError::WorkSource(format!("failed to spawn git fetch: {e}")))?;

    if !fetch.status.success() {
        let stderr = String::from_utf8_lossy(&fetch.stderr);
        if stderr.to_lowercase().contains("couldn't find remote ref") {
            return Ok(None);
        }
        return Err(error::LegionError::WorkSource(format!(
            "git fetch origin {branch} failed: {}",
            stderr.trim()
        )));
    }

    let rev = Command::new("git")
        .arg("-C")
        .arg(checkout)
        .args(["rev-parse", "FETCH_HEAD"])
        .output()
        .map_err(|e| {
            error::LegionError::WorkSource(format!("failed to spawn git rev-parse: {e}"))
        })?;
    if !rev.status.success() {
        return Err(error::LegionError::WorkSource(
            "git fetch succeeded but FETCH_HEAD did not resolve".to_string(),
        ));
    }
    Ok(Some(
        String::from_utf8_lossy(&rev.stdout).trim().to_string(),
    ))
}

/// Fetch `main` fresh into `refs/remotes/origin/main` and return a ref
/// string usable in later `git` invocations against the up-to-date remote
/// state. Falls back to the local `main` branch only when the remote fetch
/// itself fails (e.g. no network) AND a local `main` resolves -- a stale
/// local `main` would fail every survival test and turn the squash-merge
/// case this feature exists for into a spurious refusal, so the fetch is not
/// optional in the ordinary case.
fn fetch_main_ref(checkout: &Path) -> error::Result<String> {
    let fetch = Command::new("git")
        .arg("-C")
        .arg(checkout)
        .args(["fetch", "origin", "main:refs/remotes/origin/main"])
        .output()
        .map_err(|e| error::LegionError::WorkSource(format!("failed to spawn git fetch: {e}")))?;

    if fetch.status.success() {
        return Ok("refs/remotes/origin/main".to_string());
    }

    let local = Command::new("git")
        .arg("-C")
        .arg(checkout)
        .args(["rev-parse", "--verify", "main"])
        .output()
        .map_err(|e| {
            error::LegionError::WorkSource(format!("failed to spawn git rev-parse: {e}"))
        })?;
    if local.status.success() {
        return Ok("main".to_string());
    }

    Err(error::LegionError::WorkSource(format!(
        "could not resolve 'main' to check discarded commits against: fetching origin/main \
         failed ({}) and no local 'main' branch exists",
        String::from_utf8_lossy(&fetch.stderr).trim()
    )))
}

/// The best common ancestor of `a` and `b` (`git merge-base`).
fn merge_base(checkout: &Path, a: &str, b: &str) -> error::Result<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(checkout)
        .args(["merge-base", a, b])
        .output()
        .map_err(|e| {
            error::LegionError::WorkSource(format!("failed to spawn git merge-base: {e}"))
        })?;
    if !out.status.success() {
        return Err(error::LegionError::WorkSource(format!(
            "git merge-base {a} {b} failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// File paths that differ between `a` and `b` (`git diff --name-only <a>
/// <b>`), optionally restricted to `paths` (empty means the whole tree).
/// Used two ways by the cumulative survival fallback: first with an empty
/// `paths` to discover which files the discarded set touched at all, then
/// with that exact file list to check whether those specific files still
/// differ in the new state -- an empty result the second time means every
/// one of them is byte-identical, i.e. nothing was lost.
fn diff_name_only(
    checkout: &Path,
    a: &str,
    b: &str,
    paths: &[String],
) -> error::Result<Vec<String>> {
    let mut args: Vec<&str> = vec!["diff", "--name-only", a, b];
    if !paths.is_empty() {
        args.push("--");
        args.extend(paths.iter().map(String::as_str));
    }
    let out = Command::new("git")
        .arg("-C")
        .arg(checkout)
        .args(&args)
        .output()
        .map_err(|e| error::LegionError::WorkSource(format!("failed to spawn git diff: {e}")))?;
    if !out.status.success() {
        return Err(error::LegionError::WorkSource(format!(
            "git diff --name-only {a} {b} failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    Ok(String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect())
}

/// Commits reachable from `include` but not from `exclude` (`git rev-list
/// <exclude>..<include>`), as full shas.
fn rev_list_exclusive(checkout: &Path, exclude: &str, include: &str) -> error::Result<Vec<String>> {
    let range = format!("{exclude}..{include}");
    let out = Command::new("git")
        .arg("-C")
        .arg(checkout)
        .args(["rev-list", &range])
        .output()
        .map_err(|e| {
            error::LegionError::WorkSource(format!("failed to spawn git rev-list: {e}"))
        })?;
    if !out.status.success() {
        return Err(error::LegionError::WorkSource(format!(
            "git rev-list {range} failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    Ok(String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect())
}

/// The subject line of `sha` (`git log -1 --format=%s <sha>`).
fn commit_subject(checkout: &Path, sha: &str) -> error::Result<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(checkout)
        .args(["log", "-1", "--format=%s", sha])
        .output()
        .map_err(|e| error::LegionError::WorkSource(format!("failed to spawn git log: {e}")))?;
    if !out.status.success() {
        return Err(error::LegionError::WorkSource(format!(
            "git log -1 --format=%s {sha} failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Whether `sha` is reachable from `ref_name` (`git merge-base
/// --is-ancestor`). Exit code 1 means "not an ancestor" (a normal, non-error
/// result, per the command's own documented semantics); any other non-zero
/// exit (e.g. an unknown object) is a real failure.
fn is_ancestor(checkout: &Path, sha: &str, ref_name: &str) -> error::Result<bool> {
    let out = Command::new("git")
        .arg("-C")
        .arg(checkout)
        .args(["merge-base", "--is-ancestor", sha, ref_name])
        .output()
        .map_err(|e| {
            error::LegionError::WorkSource(format!("failed to spawn git merge-base: {e}"))
        })?;
    match out.status.code() {
        Some(0) => Ok(true),
        Some(1) => Ok(false),
        _ => Err(error::LegionError::WorkSource(format!(
            "git merge-base --is-ancestor {sha} {ref_name} failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ))),
    }
}

/// `git cherry <upstream> <head>`, parsed into a sha -> survived map.
///
/// Candidates are exactly the commits in `upstream..head`; each is checked
/// for patch-id equivalence against the commits in `head..upstream` (the
/// commits unique to `upstream`). A `-` mark means an equivalent change is
/// already in `upstream`'s unique history (mapped to `true` here); a `+`
/// mark means it is not (`false`). Confirmed empirically against a live
/// amend fixture before relying on this direction and format -- `git
/// cherry`'s mark direction is easy to get backwards from the manpage alone.
fn cherry_marks(
    checkout: &Path,
    upstream: &str,
    head: &str,
) -> error::Result<HashMap<String, bool>> {
    let out = Command::new("git")
        .arg("-C")
        .arg(checkout)
        .args(["cherry", upstream, head])
        .output()
        .map_err(|e| error::LegionError::WorkSource(format!("failed to spawn git cherry: {e}")))?;
    if !out.status.success() {
        return Err(error::LegionError::WorkSource(format!(
            "git cherry {upstream} {head} failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    let mut marks = HashMap::new();
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        let mut parts = line.split_whitespace();
        let Some(mark) = parts.next() else { continue };
        let Some(sha) = parts.next() else { continue };
        marks.insert(sha.to_string(), mark == "-");
    }
    Ok(marks)
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

    /// #915: the tag path must close the same crafted-value holes the branch
    /// path does. There is no force flag on this command, and a tag value must
    /// not be able to recover one either.
    #[test]
    fn validate_tag_refuses_the_same_crafted_shapes_as_branch() {
        assert!(validate_tag("--force").is_err(), "flag-shaped");
        assert!(validate_tag("-f").is_err(), "short flag");
        assert!(validate_tag("+v1.0.0").is_err(), "force refspec prefix");
        assert!(
            validate_tag("v1.0.0:refs/tags/other").is_err(),
            "refspec separator retargets the remote ref"
        );
        assert!(validate_tag("").is_err(), "empty");
        assert!(validate_tag("v1 0").is_err(), "whitespace");
    }

    #[test]
    fn validate_tag_accepts_ordinary_tag_names() {
        assert!(validate_tag("v0.28.0").is_ok());
        assert!(validate_tag("v0.0.79").is_ok());
        assert!(validate_tag("release-2026-08-14").is_ok());
    }

    /// Unlike a branch, a tag named `main` is merely unwise rather than the
    /// hazard `validate_branch` guards -- agents never push the main BRANCH,
    /// but a tag that happens to be called main pushes a tag ref, not a
    /// branch ref. Pinned so nobody "fixes" the asymmetry by copying the
    /// REFUSED_BRANCHES check across without asking what it was for.
    #[test]
    fn validate_tag_does_not_inherit_the_main_master_refusal() {
        assert!(validate_tag("main").is_ok());
        assert!(validate_tag("master").is_ok());
    }

    // -- #1172: --force -----------------------------------------------------

    #[test]
    fn survival_verdict_survives_and_label() {
        assert!(SurvivalVerdict::PresentInNewHistory.survives());
        assert!(SurvivalVerdict::AlreadyInMainBySha.survives());
        assert!(SurvivalVerdict::AlreadyInMainByPatchId.survives());
        assert!(!SurvivalVerdict::Orphan.survives());

        assert_eq!(
            SurvivalVerdict::PresentInNewHistory.label(),
            "present-in-new-history"
        );
        assert_eq!(SurvivalVerdict::Orphan.label(), "orphan");
    }

    #[test]
    fn orphan_refusal_names_each_commit_as_sha_and_subject() {
        let orphans = [
            DiscardedCommit {
                sha: "deadbeef".to_string(),
                subject: "add the thing".to_string(),
                verdict: SurvivalVerdict::Orphan,
            },
            DiscardedCommit {
                sha: "cafef00d".to_string(),
                subject: "fix the thing".to_string(),
                verdict: SurvivalVerdict::Orphan,
            },
        ];
        let refs: Vec<&DiscardedCommit> = orphans.iter().collect();
        let err = orphan_refusal("feat/x", &refs);
        match err {
            error::LegionError::PushRefused { branch, reason } => {
                assert_eq!(branch, "feat/x");
                assert!(reason.contains("deadbeef add the thing"));
                assert!(reason.contains("cafef00d fix the thing"));
                assert!(reason.contains("--force-reason"));
            }
            other => panic!("expected PushRefused, got {other:?}"),
        }
    }

    /// The audit row is the load-bearing part of #1172 (a force-push that
    /// leaves no record of what it discarded is exactly what the analysis
    /// exists to prevent) -- pin its shape directly rather than only via the
    /// PushRefused message text.
    #[test]
    fn force_audit_details_carries_old_new_discarded_and_reason() {
        let analysis = ForceAnalysis {
            old_remote_sha: Some("oldsha".to_string()),
            discarded: vec![
                DiscardedCommit {
                    sha: "sha1".to_string(),
                    subject: "subject one".to_string(),
                    verdict: SurvivalVerdict::PresentInNewHistory,
                },
                DiscardedCommit {
                    sha: "sha2".to_string(),
                    subject: "subject two".to_string(),
                    verdict: SurvivalVerdict::AlreadyInMainByPatchId,
                },
            ],
        };
        let details = force_audit_details(&analysis, "newsha", Some("verified by hand"));
        let parsed: serde_json::Value = serde_json::from_str(&details).expect("valid JSON");

        assert_eq!(parsed["forced"], true);
        assert_eq!(parsed["old_remote_sha"], "oldsha");
        assert_eq!(parsed["new_sha"], "newsha");
        assert_eq!(parsed["discarded_count"], 2);
        assert_eq!(parsed["force_reason"], "verified by hand");
        assert_eq!(parsed["discarded"][0]["sha"], "sha1");
        assert_eq!(parsed["discarded"][0]["survival"], "present-in-new-history");
        assert_eq!(parsed["discarded"][1]["sha"], "sha2");
        assert_eq!(
            parsed["discarded"][1]["survival"],
            "already-in-main-by-patch-id"
        );
    }

    #[test]
    fn force_audit_details_no_remote_and_no_reason_are_null() {
        let analysis = ForceAnalysis {
            old_remote_sha: None,
            discarded: Vec::new(),
        };
        let details = force_audit_details(&analysis, "newsha", None);
        let parsed: serde_json::Value = serde_json::from_str(&details).expect("valid JSON");
        assert!(parsed["old_remote_sha"].is_null());
        assert!(parsed["force_reason"].is_null());
        assert_eq!(parsed["discarded_count"], 0);
    }

    // -- #1172: real-git fixtures for the helpers `--force` is built on ----
    //
    // These are plain, disconnected `git init` tempdirs (never `git worktree
    // add`), so they carry none of the config-inheritance hazard #723 guards
    // against in tests/integration/common.rs -- but every invocation still
    // pins GIT_CONFIG_GLOBAL/GIT_CONFIG_SYSTEM to isolated empty files so a
    // real global `core.hooksPath` (the nested-claude pre-push review) can
    // never fire against a throwaway fixture repo.

    fn fixture_git_env(cmd: &mut Command) {
        let dir = tempfile::tempdir().expect("create isolated git config dir");
        let global = dir.path().join("global.gitconfig");
        let system = dir.path().join("system.gitconfig");
        std::fs::write(&global, "").expect("write isolated global gitconfig");
        std::fs::write(&system, "").expect("write isolated system gitconfig");
        cmd.env("GIT_CONFIG_GLOBAL", &global)
            .env("GIT_CONFIG_SYSTEM", &system);
        // The tempdir must outlive the spawned git process; leaked
        // deliberately, same trade-off tests/integration/common.rs makes for
        // its suite-wide equivalent.
        std::mem::forget(dir);
    }

    /// Run `git` against a fixture repo, requiring success. Identity is
    /// supplied on writes via the caller's own `-c` args (never a `git
    /// config` write), matching `run_git_fixture`'s discipline in the
    /// integration test crate.
    fn git_fixture(dir: &Path, args: &[&str]) -> String {
        let mut cmd = Command::new("git");
        cmd.arg("-C").arg(dir);
        fixture_git_env(&mut cmd);
        let out = cmd
            .args(args)
            .output()
            .unwrap_or_else(|e| panic!("git {args:?} failed to spawn in {dir:?}: {e}"));
        assert!(
            out.status.success(),
            "git {args:?} exited non-zero in {dir:?}\nstderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    /// [`git_fixture`] with a baked-in throwaway identity, for the
    /// mutating commands (`commit`) that need one.
    fn git_fixture_id(dir: &Path, args: &[&str]) -> String {
        let mut full: Vec<&str> = vec![
            "-c",
            "user.name=Legion Test Fixture",
            "-c",
            "user.email=legion-test-fixture@example.invalid",
            "-c",
            "commit.gpgsign=false",
        ];
        full.extend_from_slice(args);
        git_fixture(dir, &full)
    }

    fn init_bare_remote_fixture() -> tempfile::TempDir {
        let remote = tempfile::tempdir().unwrap();
        git_fixture(remote.path(), &["init", "--bare", "-q"]);
        remote
    }

    /// `git cherry`'s mark direction confirmed empirically (see
    /// [`cherry_marks`]'s doc comment) against a live amend: `git cherry
    /// <new> <old>` marks the pre-amend commit `-` (patch-equivalent to the
    /// post-amend commit). Pinned here as a regression test so a future
    /// change to the survival analysis cannot silently invert this without
    /// a test failing.
    #[test]
    fn cherry_marks_finds_amended_commit_as_patch_equivalent() {
        let local = tempfile::tempdir().unwrap();
        let lp = local.path();
        git_fixture(lp, &["init", "-q", "-b", "main"]);
        std::fs::write(lp.join("a.txt"), "a\n").unwrap();
        git_fixture_id(lp, &["add", "a.txt"]);
        git_fixture_id(lp, &["commit", "-q", "-m", "A"]);

        std::fs::write(lp.join("b.txt"), "b\n").unwrap();
        git_fixture_id(lp, &["add", "b.txt"]);
        git_fixture_id(lp, &["commit", "-q", "-m", "B"]);
        let old_tip = git_fixture(lp, &["rev-parse", "HEAD"]);

        git_fixture_id(lp, &["commit", "--amend", "-q", "-m", "B amended"]);
        let new_tip = git_fixture(lp, &["rev-parse", "HEAD"]);

        let marks = cherry_marks(lp, &new_tip, &old_tip).expect("cherry must run");
        assert_eq!(
            marks.get(&old_tip),
            Some(&true),
            "the pre-amend commit must be marked as patch-equivalent to the amended one"
        );
    }

    /// The stale-lease path (#1172), exercised against a real `git push
    /// --force-with-lease` rejection rather than a stubbed one: a bare
    /// remote, a local checkout that pushes a commit, a SECOND push to the
    /// same remote that lands after we captured the sha we intend to lease
    /// against (simulating a concurrent push by another agent/node), and
    /// then `run_push_force` with the now-stale sha. This is `analyze_force_
    /// push` -> `run_push_force`'s own internal boundary, called directly
    /// because the full `legion push --force` CLI path fetches immediately
    /// before pushing with no seam to interpose a concurrent push between
    /// the two -- see the work summary for why the CLI-level case is not
    /// separately covered.
    #[test]
    fn run_push_force_reports_stale_lease_and_names_the_actual_sha() {
        let remote = init_bare_remote_fixture();
        let local = tempfile::tempdir().unwrap();
        let lp = local.path();

        git_fixture(lp, &["init", "-q", "-b", "main"]);
        git_fixture(
            lp,
            &["remote", "add", "origin", remote.path().to_str().unwrap()],
        );
        std::fs::write(lp.join("seed.txt"), "seed\n").unwrap();
        git_fixture_id(lp, &["add", "seed.txt"]);
        git_fixture_id(lp, &["commit", "-q", "-m", "seed"]);
        git_fixture(lp, &["push", "-q", "origin", "main"]);

        git_fixture(lp, &["checkout", "-q", "-b", "topic"]);
        std::fs::write(lp.join("t1.txt"), "t1\n").unwrap();
        git_fixture_id(lp, &["add", "t1.txt"]);
        git_fixture_id(lp, &["commit", "-q", "-m", "T1"]);
        git_fixture(lp, &["push", "-q", "origin", "topic"]);
        // The sha we will (deliberately) lease a force-push against, as if
        // this were captured by an earlier `legion push --force` fetch.
        let stale_expected = git_fixture(lp, &["rev-parse", "HEAD"]);

        // A concurrent push lands on the remote AFTER `stale_expected` was
        // captured -- an ordinary fast-forward push, same checkout for
        // fixture simplicity, but the remote-side effect is identical to a
        // different agent/node pushing.
        std::fs::write(lp.join("t2.txt"), "t2\n").unwrap();
        git_fixture_id(lp, &["add", "t2.txt"]);
        git_fixture_id(lp, &["commit", "-q", "-m", "T2 concurrent"]);
        git_fixture(lp, &["push", "-q", "origin", "topic"]);
        let current_remote = git_fixture(lp, &["rev-parse", "HEAD"]);

        // Rewrite history locally so there is something that actually needs
        // a force-push.
        git_fixture_id(
            lp,
            &["commit", "--amend", "-q", "-m", "T2 concurrent, amended"],
        );

        let result = run_push_force(lp, "topic", &stale_expected);
        match result {
            Err(error::LegionError::PushLeaseStale {
                branch,
                expected,
                actual,
            }) => {
                assert_eq!(branch, "topic");
                assert_eq!(expected, stale_expected);
                assert_eq!(
                    actual, current_remote,
                    "the reported actual sha must be the remote's real current tip"
                );
            }
            other => panic!("expected PushLeaseStale, got {other:?}"),
        }
    }
}
