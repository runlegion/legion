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

pub(crate) fn handle_push(repo: String, branch: Option<String>) -> error::Result<()> {
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

    info!(
        "[legion] pushing '{target_branch}' from {}",
        checkout_path.display()
    );

    let push_result = run_push(&checkout_path, &target_branch);

    // Audit every attempt, success or failure -- the audit trail is the
    // point of routing pushes through this command instead of raw `git
    // push`, so a hook-blocked push must leave a row just as a successful
    // one does. The error (if any) propagates AFTER the row is written.
    let database = open_db()?;
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
}
