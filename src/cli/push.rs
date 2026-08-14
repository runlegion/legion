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
) -> error::Result<()> {
    if let Some(t) = tag {
        return handle_push_tag(repo, t);
    }
    handle_push_branch(repo, branch)
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

fn handle_push_branch(repo: String, branch: Option<String>) -> error::Result<()> {
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
}
