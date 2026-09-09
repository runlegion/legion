//! Integration tests for `legion push` (#791): the sanctioned in-band push
//! path. Every test drives real `git` fixtures (a bare "remote" plus a
//! local checkout, mirroring `setup_git_repo_with_feature_branch` in
//! worksource_pr.rs) since the command's whole job is resolving worktrees
//! and shelling out to `git push`.

use crate::common::*;
use std::path::Path;

/// Init a bare "remote" repo. Plain `git init --bare` never reads or writes
/// any config, and this is a disconnected tempdir with no relation to the
/// enclosing real checkout, so it needs none of `run_git_fixture`'s
/// isolation machinery (that exists specifically for the `git worktree
/// add` config-inheritance hazard, #723).
fn init_bare_remote() -> tempfile::TempDir {
    let remote = tempfile::tempdir().unwrap();
    let out = std::process::Command::new("git")
        .current_dir(remote.path())
        .args(["init", "--bare", "-q"])
        .output()
        .expect("git init --bare must spawn");
    assert!(
        out.status.success(),
        "git init --bare failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    remote
}

/// Local repo with `main` (seeded and pushed to `origin`) and a feature
/// branch `feat/x` carrying one additional commit. Leaves the checkout on
/// `feat/x`. Every write goes through `run_git_fixture` (#723 isolation).
fn setup_local_repo(remote: &Path) -> tempfile::TempDir {
    let local = tempfile::tempdir().unwrap();
    let lp = local.path();
    run_git_fixture(lp, &["init", "-q", "-b", "main"]);
    run_git_fixture(lp, &["remote", "add", "origin", remote.to_str().unwrap()]);

    std::fs::write(lp.join("README.md"), "seed\n").unwrap();
    run_git_fixture(lp, &["add", "README.md"]);
    run_git_fixture(lp, &["commit", "-q", "-m", "seed"]);
    run_git_fixture(lp, &["push", "-q", "origin", "main"]);

    run_git_fixture(lp, &["checkout", "-q", "-b", "feat/x"]);
    std::fs::write(lp.join("feature.txt"), "change\n").unwrap();
    run_git_fixture(lp, &["add", "feature.txt"]);
    run_git_fixture(lp, &["commit", "-q", "-m", "add feature"]);

    local
}

/// `legion push` command scoped to `cwd`, with `GIT_CONFIG_GLOBAL`/
/// `GIT_CONFIG_SYSTEM` pinned to isolated empty files. This is layered onto
/// the command UNDER TEST (not just fixture setup) because `legion push`
/// shells out to a real `git push`, and an operator machine's real global
/// config could point `core.hooksPath` at a real pre-push hook (the
/// nested-claude review) -- isolating it keeps these tests hermetic and
/// fast regardless of the host's global git config.
fn push_cmd(data_dir: &Path, cwd: &Path) -> std::process::Command {
    let (global, system) = isolated_git_config_paths();
    let mut cmd = legion_cmd(data_dir);
    cmd.current_dir(cwd)
        .env("GIT_CONFIG_GLOBAL", global)
        .env("GIT_CONFIG_SYSTEM", system);
    cmd
}

fn rev_parse(repo: &Path, rev: &str) -> std::process::Output {
    std::process::Command::new("git")
        .current_dir(repo)
        .args(["rev-parse", rev])
        .output()
        .expect("git rev-parse must spawn")
}

/// Every `push` audit row's `details` field, parsed as JSON (the CLI's
/// `details` column is itself a JSON string, so this parses it twice: once
/// for the outer `audit --json` array, once for each row's `details`
/// string). Oldest first, so index `[0]` is the first attempt.
///
/// Parsing the JSON structurally, rather than substring-matching the raw
/// `--json` text, is deliberate: the outer array is pretty-printed (space
/// after `:`) while `details` -- a JSON string embedded in that pretty
/// output -- carries its own backslash-escaped quotes, so a naive
/// `contains("\"key\":value")` check silently never matches either form.
/// The returned `Value` is the row's parsed `details` object with an
/// `"outcome"` field merged in from the outer row, so a caller can assert on
/// both from one value.
fn audit_details_for(data_dir: &Path, target_ref: &str) -> Vec<serde_json::Value> {
    let audit_out = run_ok(legion_cmd(data_dir).args(["audit", "--action", "push", "--json"]));
    let rows: Vec<serde_json::Value> =
        serde_json::from_str(&audit_out).expect("audit --json must produce a JSON array");
    let mut matching: Vec<(String, serde_json::Value)> = rows
        .into_iter()
        .filter(|row| row["target_ref"] == target_ref)
        .map(|row| {
            let timestamp = row["timestamp"].as_str().unwrap_or_default().to_string();
            let details_str = row["details"].as_str().unwrap_or_default();
            let mut details: serde_json::Value =
                serde_json::from_str(details_str).unwrap_or_else(|e| {
                    panic!("audit row details did not parse as JSON: {details_str:?}: {e}")
                });
            details["outcome"] = row["outcome"].clone();
            (timestamp, details)
        })
        .collect();
    matching.sort_by(|a, b| a.0.cmp(&b.0));
    matching.into_iter().map(|(_, d)| d).collect()
}

/// Happy path: pushing a feature branch from the checkout that has it
/// checked out succeeds, lands the ref on the remote, and sets the
/// upstream tracking branch (`-u`).
#[cfg(unix)]
#[test]
fn push_first_time_sets_upstream_and_pushes_feature_branch() {
    let _guard = RealRepoConfigGuard::new();
    let remote = init_bare_remote();
    let local = setup_local_repo(remote.path());
    let data_dir = tempfile::tempdir().unwrap();

    let stdout = run_ok(push_cmd(data_dir.path(), local.path()).args([
        "push",
        "--repo",
        "test-agent",
        "--branch",
        "feat/x",
    ]));
    assert!(
        stdout.contains("feat/x"),
        "expected confirmation naming the branch, got: {stdout}"
    );

    let rev = rev_parse(remote.path(), "refs/heads/feat/x");
    assert!(
        rev.status.success(),
        "expected feat/x to exist on the remote after push"
    );

    let upstream = std::process::Command::new("git")
        .current_dir(local.path())
        .args(["rev-parse", "--abbrev-ref", "feat/x@{upstream}"])
        .output()
        .expect("git rev-parse --abbrev-ref must spawn");
    assert!(
        upstream.status.success(),
        "expected -u to set the upstream tracking branch"
    );
    assert_eq!(
        String::from_utf8_lossy(&upstream.stdout).trim(),
        "origin/feat/x"
    );
}

/// Omitting `--branch` pushes whatever branch the CWD has checked out.
#[cfg(unix)]
#[test]
fn push_default_branch_uses_cwd_checked_out_branch() {
    let _guard = RealRepoConfigGuard::new();
    let remote = init_bare_remote();
    let local = setup_local_repo(remote.path()); // leaves checkout on feat/x

    let data_dir = tempfile::tempdir().unwrap();
    let stdout =
        run_ok(push_cmd(data_dir.path(), local.path()).args(["push", "--repo", "test-agent"]));
    assert!(
        stdout.contains("feat/x"),
        "expected the default (CWD-checked-out) branch feat/x to be pushed, got: {stdout}"
    );
    assert!(
        rev_parse(remote.path(), "refs/heads/feat/x")
            .status
            .success()
    );
}

/// `main` is refused outright, before any worktree resolution or push
/// attempt.
#[cfg(unix)]
#[test]
fn push_refuses_main() {
    let remote = init_bare_remote();
    let local = setup_local_repo(remote.path());
    let data_dir = tempfile::tempdir().unwrap();

    let (_stdout, stderr) = run_fail(push_cmd(data_dir.path(), local.path()).args([
        "push",
        "--repo",
        "test-agent",
        "--branch",
        "main",
    ]));
    assert!(
        stderr.contains("main") && stderr.to_lowercase().contains("refus"),
        "expected a refusal naming main, got: {stderr}"
    );
}

/// `master` is refused the same way as `main`.
#[cfg(unix)]
#[test]
fn push_refuses_master() {
    let remote = init_bare_remote();
    let local = setup_local_repo(remote.path());
    run_git_fixture(local.path(), &["checkout", "-q", "-b", "master"]);
    let data_dir = tempfile::tempdir().unwrap();

    let (_stdout, stderr) = run_fail(push_cmd(data_dir.path(), local.path()).args([
        "push",
        "--repo",
        "test-agent",
        "--branch",
        "master",
    ]));
    assert!(
        stderr.contains("master") && stderr.to_lowercase().contains("refus"),
        "expected a refusal naming master, got: {stderr}"
    );
}

/// A `--branch` value shaped like a git flag or a force/retarget refspec is
/// refused -- force-pushing goes through the audited `--force` flag (#1172),
/// never a crafted branch value.
#[cfg(unix)]
#[test]
fn push_refuses_flag_shaped_branch_value() {
    let remote = init_bare_remote();
    let local = setup_local_repo(remote.path());
    let data_dir = tempfile::tempdir().unwrap();

    // `=`-form so clap assigns the literal value rather than trying to
    // parse `--force` as a separate flag token.
    let (_stdout, stderr) = run_fail(push_cmd(data_dir.path(), local.path()).args([
        "push",
        "--repo",
        "test-agent",
        "--branch=--force",
    ]));
    assert!(
        stderr.contains("not a plain branch name"),
        "expected the flag-shaped-value refusal, got: {stderr}"
    );
}

/// Requesting a branch that no checkout of the repo has results in a hard
/// error naming the worktrees that were searched.
#[cfg(unix)]
#[test]
fn push_branch_not_found_in_any_worktree_errors() {
    let remote = init_bare_remote();
    let local = setup_local_repo(remote.path());
    let data_dir = tempfile::tempdir().unwrap();

    let (_stdout, stderr) = run_fail(push_cmd(data_dir.path(), local.path()).args([
        "push",
        "--repo",
        "test-agent",
        "--branch",
        "feat/does-not-exist",
    ]));
    assert!(
        stderr.contains("feat/does-not-exist"),
        "expected the missing branch name in the error, got: {stderr}"
    );
    assert!(
        stderr.contains(local.path().to_str().unwrap()),
        "expected the searched checkout path named in the error, got: {stderr}"
    );
}

/// Resolves the checkout that has the target branch checked out even when
/// that is NOT the checkout `legion push` was invoked from -- the core
/// push-from-own-checkout doctrine (#791, 019f20eb). CWD sits on `main`; a
/// linked worktree sits on `feat/x`; pushing `feat/x` must succeed by
/// finding and pushing FROM the linked worktree.
#[cfg(unix)]
#[test]
fn push_resolves_checkout_from_linked_worktree_not_cwd() {
    let _guard = RealRepoConfigGuard::new();
    let remote = init_bare_remote();
    let local = setup_local_repo(remote.path());
    // Back to main in the primary checkout -- it no longer has feat/x.
    run_git_fixture(local.path(), &["checkout", "-q", "main"]);

    // A separate, not-yet-existing path for the linked worktree.
    let linked_parent = tempfile::tempdir().unwrap();
    let linked_path = linked_parent.path().join("linked-checkout");
    run_git_fixture(
        local.path(),
        &["worktree", "add", linked_path.to_str().unwrap(), "feat/x"],
    );

    let data_dir = tempfile::tempdir().unwrap();
    // Invoke from `local` (checked out to main) targeting feat/x, which
    // only the linked worktree has.
    let stdout = run_ok(push_cmd(data_dir.path(), local.path()).args([
        "push",
        "--repo",
        "test-agent",
        "--branch",
        "feat/x",
    ]));
    assert!(stdout.contains("feat/x"), "got: {stdout}");

    assert!(
        rev_parse(remote.path(), "refs/heads/feat/x")
            .status
            .success(),
        "expected feat/x to have been pushed from the linked worktree"
    );
}

/// Every push attempt is audit-logged, success or failure, carrying the
/// branch and the resolved checkout.
#[cfg(unix)]
#[test]
fn push_writes_audit_row_on_success() {
    let remote = init_bare_remote();
    let local = setup_local_repo(remote.path());
    let data_dir = tempfile::tempdir().unwrap();

    run_ok(push_cmd(data_dir.path(), local.path()).args([
        "push",
        "--repo",
        "test-agent",
        "--branch",
        "feat/x",
    ]));

    let audit_out =
        run_ok(legion_cmd(data_dir.path()).args(["audit", "--action", "push", "--json"]));
    assert!(
        audit_out.contains("\"action\": \"push\""),
        "got: {audit_out}"
    );
    assert!(audit_out.contains("feat/x"), "got: {audit_out}");
    assert!(
        audit_out.contains("\"outcome\": \"success\""),
        "got: {audit_out}"
    );
    assert!(
        audit_out.contains(local.path().to_str().unwrap()),
        "expected the resolved checkout path in the audit details, got: {audit_out}"
    );
}

/// An underlying `git push` failure (here: `origin` points at a directory
/// that is not a git repository at all) surfaces as the command's error
/// with git's own stderr relayed, and the failed attempt is still
/// audit-logged with outcome "failure" -- the audit trail is the whole
/// point of routing pushes through this command.
#[cfg(unix)]
#[test]
fn push_underlying_git_failure_surfaces_error_and_audits_failure() {
    let local = tempfile::tempdir().unwrap();
    let lp = local.path();
    run_git_fixture(lp, &["init", "-q", "-b", "main"]);

    let bogus_remote = tempfile::tempdir().unwrap();
    run_git_fixture(
        lp,
        &[
            "remote",
            "add",
            "origin",
            bogus_remote.path().to_str().unwrap(),
        ],
    );

    std::fs::write(lp.join("README.md"), "seed\n").unwrap();
    run_git_fixture(lp, &["add", "README.md"]);
    run_git_fixture(lp, &["commit", "-q", "-m", "seed"]);
    run_git_fixture(lp, &["checkout", "-q", "-b", "feat/x"]);
    std::fs::write(lp.join("feature.txt"), "change\n").unwrap();
    run_git_fixture(lp, &["add", "feature.txt"]);
    run_git_fixture(lp, &["commit", "-q", "-m", "add feature"]);

    let data_dir = tempfile::tempdir().unwrap();
    let (_stdout, stderr) = run_fail(push_cmd(data_dir.path(), lp).args([
        "push",
        "--repo",
        "test-agent",
        "--branch",
        "feat/x",
    ]));
    assert!(
        !stderr.trim().is_empty(),
        "expected git's own failure text relayed to stderr"
    );

    let audit_out =
        run_ok(legion_cmd(data_dir.path()).args(["audit", "--action", "push", "--json"]));
    assert!(
        audit_out.contains("\"outcome\": \"failure\""),
        "expected a failure-outcome audit row for the failed push, got: {audit_out}"
    );
}

// ---------------------------------------------------------------------------
// #1172: `legion push --force`, gated on the discarded-commit survival
// analysis.
// ---------------------------------------------------------------------------

/// Done-When #1: a branch rebased onto a squash-merged base pushes with
/// `--force` and no `--force-reason`, because every discarded commit is
/// patch-equivalent to something in the new state -- the predecessor's
/// pre-squash commit matches `main`'s squash commit by patch-id, and the
/// branch's own commit matches its rebased replay by patch-id.
///
/// Also covers Done-When #5 (the audit row): asserts the discarded commits
/// and their survival verdicts, plus old/new sha, land in the audit details.
#[cfg(unix)]
#[test]
fn push_force_survives_rebase_onto_squash_merged_base_with_no_reason() {
    let _guard = RealRepoConfigGuard::new();
    let remote = init_bare_remote();
    let local = tempfile::tempdir().unwrap();
    let lp = local.path();

    run_git_fixture(lp, &["init", "-q", "-b", "main"]);
    run_git_fixture(
        lp,
        &["remote", "add", "origin", remote.path().to_str().unwrap()],
    );
    std::fs::write(lp.join("seed.txt"), "seed\n").unwrap();
    run_git_fixture(lp, &["add", "seed.txt"]);
    run_git_fixture(lp, &["commit", "-q", "-m", "seed"]);
    run_git_fixture(lp, &["push", "-q", "origin", "main"]);

    // Predecessor branch: one commit, never itself pushed (mirroring the
    // #1167->#1168 scenario: only the SUCCESSOR branch had been pushed,
    // carrying the predecessor's commit as an ancestor via `git merge`).
    run_git_fixture(lp, &["checkout", "-q", "-b", "predecessor"]);
    std::fs::write(lp.join("predecessor.txt"), "predecessor content\n").unwrap();
    run_git_fixture(lp, &["add", "predecessor.txt"]);
    run_git_fixture(lp, &["commit", "-q", "-m", "predecessor feature"]);

    // Stacked branch: predecessor's commit as an ancestor, plus its own.
    run_git_fixture(lp, &["checkout", "-q", "-b", "topic"]);
    std::fs::write(lp.join("topic.txt"), "topic content\n").unwrap();
    run_git_fixture(lp, &["add", "topic.txt"]);
    run_git_fixture(lp, &["commit", "-q", "-m", "topic feature"]);
    run_git_fixture(lp, &["push", "-q", "origin", "topic"]);
    let old_remote_sha = run_git_fixture_output(lp, &["rev-parse", "HEAD"]);

    // Simulate a GitHub squash-merge of the (single-commit) predecessor PR:
    // the SAME diff lands on main as one new commit with a different sha and
    // message -- identical diff means identical `git patch-id`, regardless
    // of commit metadata.
    run_git_fixture(lp, &["checkout", "-q", "main"]);
    std::fs::write(lp.join("predecessor.txt"), "predecessor content\n").unwrap();
    run_git_fixture(lp, &["add", "predecessor.txt"]);
    run_git_fixture(
        lp,
        &["commit", "-q", "-m", "squash: predecessor feature (#1000)"],
    );
    run_git_fixture(lp, &["push", "-q", "origin", "main"]);

    // Rebuild `topic` onto the squashed `main`, replaying only its own
    // commit (dropping the now-redundant predecessor ancestor) -- the
    // recovery this issue exists to make pushable.
    run_git_fixture(lp, &["checkout", "-q", "topic"]);
    run_git_fixture(
        lp,
        &["rebase", "-q", "--onto", "main", "predecessor", "topic"],
    );
    let new_local_sha = run_git_fixture_output(lp, &["rev-parse", "HEAD"]);
    assert_ne!(
        old_remote_sha, new_local_sha,
        "the rebase must have produced a new sha"
    );

    let data_dir = tempfile::tempdir().unwrap();
    let stdout = run_ok(push_cmd(data_dir.path(), lp).args([
        "push",
        "--repo",
        "test-agent",
        "--branch",
        "topic",
        "--force",
    ]));
    assert!(
        stdout.contains("topic"),
        "expected confirmation naming the branch, got: {stdout}"
    );

    let remote_topic = rev_parse(remote.path(), "refs/heads/topic");
    assert!(
        remote_topic.status.success(),
        "expected topic to still exist on the remote after the force-push"
    );
    assert_eq!(
        String::from_utf8_lossy(&remote_topic.stdout).trim(),
        new_local_sha,
        "expected the remote to carry the rebased sha after the force-push"
    );

    let details = audit_details_for(data_dir.path(), "topic");
    assert_eq!(
        details.len(),
        1,
        "expected exactly one push attempt: {details:?}"
    );
    let d = &details[0];
    assert_eq!(d["outcome"], "success", "{d:?}");
    assert_eq!(d["forced"], true);
    assert_eq!(d["old_remote_sha"], old_remote_sha, "{d:?}");
    assert_eq!(d["new_sha"], new_local_sha, "{d:?}");
    assert_eq!(d["discarded_count"], 2, "{d:?}");
    assert!(d["force_reason"].is_null(), "{d:?}");

    let discarded = d["discarded"]
        .as_array()
        .expect("discarded must be an array");
    assert_eq!(discarded.len(), 2, "{discarded:?}");
    let verdict_for = |sha: &str| -> String {
        discarded
            .iter()
            .find(|c| c["sha"] == sha)
            .unwrap_or_else(|| panic!("commit {sha} missing from discarded list: {discarded:?}"))
            ["survival"]
            .as_str()
            .expect("survival must be a string")
            .to_string()
    };
    assert_eq!(
        verdict_for(&old_remote_sha),
        "present-in-new-history",
        "topic's own commit should survive by patch-id match to its rebased replay"
    );
    let predecessor_sha = run_git_fixture_output(lp, &["rev-parse", "predecessor"]);
    assert_eq!(
        verdict_for(&predecessor_sha),
        "already-in-main-by-patch-id",
        "the predecessor's pre-squash commit should survive via main's squash commit"
    );
}

/// The actual incident #1172 was filed for: #1167 squash-merged THREE
/// commits into one main commit (ee5deb2e, a66616e9, 3a2b9764 -> 1e71d611
/// in the real history). `git patch-id` is computed per-commit, so none of
/// three individually patch-id-matches a squash combining all three --
/// the per-commit `AlreadyInMainByPatchId` test alone would wrongly orphan
/// every one of them despite their content being fully upstream. This is
/// the cumulative-diff fallback's reason to exist; the single-commit-squash
/// test above does NOT exercise it (a one-commit squash's diff equals the
/// squash commit's diff directly, so the per-commit test alone already
/// passes it -- that is why this needs its own, shaped-like-the-incident
/// fixture rather than trusting the one-commit case to generalize).
#[cfg(unix)]
#[test]
fn push_force_survives_three_commit_squash_merge_via_cumulative_fallback() {
    let _guard = RealRepoConfigGuard::new();
    let remote = init_bare_remote();
    let local = tempfile::tempdir().unwrap();
    let lp = local.path();

    run_git_fixture(lp, &["init", "-q", "-b", "main"]);
    run_git_fixture(
        lp,
        &["remote", "add", "origin", remote.path().to_str().unwrap()],
    );
    std::fs::write(lp.join("seed.txt"), "seed\n").unwrap();
    run_git_fixture(lp, &["add", "seed.txt"]);
    run_git_fixture(lp, &["commit", "-q", "-m", "seed"]);
    run_git_fixture(lp, &["push", "-q", "origin", "main"]);

    // Predecessor branch: THREE commits, each touching its own file, never
    // itself pushed (mirroring #1167->#1168: only the successor branch had
    // been pushed, carrying these as ancestors via `git merge`).
    run_git_fixture(lp, &["checkout", "-q", "-b", "predecessor"]);
    std::fs::write(lp.join("a.txt"), "a\n").unwrap();
    run_git_fixture(lp, &["add", "a.txt"]);
    run_git_fixture(lp, &["commit", "-q", "-m", "predecessor part 1"]);
    std::fs::write(lp.join("b.txt"), "b\n").unwrap();
    run_git_fixture(lp, &["add", "b.txt"]);
    run_git_fixture(lp, &["commit", "-q", "-m", "predecessor part 2"]);
    std::fs::write(lp.join("c.txt"), "c\n").unwrap();
    run_git_fixture(lp, &["add", "c.txt"]);
    run_git_fixture(lp, &["commit", "-q", "-m", "predecessor part 3"]);
    let c1_sha = run_git_fixture_output(lp, &["rev-parse", "predecessor~2"]);
    let c2_sha = run_git_fixture_output(lp, &["rev-parse", "predecessor~1"]);
    let c3_sha = run_git_fixture_output(lp, &["rev-parse", "predecessor"]);

    // Stacked branch: predecessor's three commits as ancestors, plus its own.
    run_git_fixture(lp, &["checkout", "-q", "-b", "topic"]);
    std::fs::write(lp.join("topic.txt"), "topic content\n").unwrap();
    run_git_fixture(lp, &["add", "topic.txt"]);
    run_git_fixture(lp, &["commit", "-q", "-m", "topic feature"]);
    run_git_fixture(lp, &["push", "-q", "origin", "topic"]);
    let old_remote_sha = run_git_fixture_output(lp, &["rev-parse", "HEAD"]);

    // Simulate a GitHub squash-merge of the THREE-commit predecessor PR: all
    // three files land in ONE new commit on main. No individual pre-squash
    // commit's diff equals this combined diff -- patch-id is per-commit.
    run_git_fixture(lp, &["checkout", "-q", "main"]);
    std::fs::write(lp.join("a.txt"), "a\n").unwrap();
    std::fs::write(lp.join("b.txt"), "b\n").unwrap();
    std::fs::write(lp.join("c.txt"), "c\n").unwrap();
    run_git_fixture(lp, &["add", "a.txt", "b.txt", "c.txt"]);
    run_git_fixture(
        lp,
        &[
            "commit",
            "-q",
            "-m",
            "squash: predecessor feature, 3 commits (#1000)",
        ],
    );
    run_git_fixture(lp, &["push", "-q", "origin", "main"]);

    // Rebuild `topic` onto the squashed `main`, replaying only its own
    // commit.
    run_git_fixture(lp, &["checkout", "-q", "topic"]);
    run_git_fixture(
        lp,
        &["rebase", "-q", "--onto", "main", "predecessor", "topic"],
    );
    let new_local_sha = run_git_fixture_output(lp, &["rev-parse", "HEAD"]);

    let data_dir = tempfile::tempdir().unwrap();
    let stdout = run_ok(push_cmd(data_dir.path(), lp).args([
        "push",
        "--repo",
        "test-agent",
        "--branch",
        "topic",
        "--force",
    ]));
    assert!(
        stdout.contains("topic"),
        "expected the push to succeed with NO --force-reason -- if this failed, the \
         cumulative-diff fallback did not clear the 3-commit squash, and the feature does \
         not solve the problem it was built for. got: {stdout}"
    );

    let remote_topic = rev_parse(remote.path(), "refs/heads/topic");
    assert_eq!(
        String::from_utf8_lossy(&remote_topic.stdout).trim(),
        new_local_sha,
        "expected the remote to carry the rebased sha after the force-push"
    );

    let details = audit_details_for(data_dir.path(), "topic");
    assert_eq!(
        details.len(),
        1,
        "expected exactly one push attempt: {details:?}"
    );
    let d = &details[0];
    assert_eq!(d["outcome"], "success", "{d:?}");
    assert!(
        d["force_reason"].is_null(),
        "no --force-reason should have been needed: {d:?}"
    );
    assert_eq!(
        d["discarded_count"], 4,
        "expected all 3 predecessor commits plus topic's own: {d:?}"
    );

    let discarded = d["discarded"]
        .as_array()
        .expect("discarded must be an array");
    let verdict_for = |sha: &str| -> String {
        discarded
            .iter()
            .find(|c| c["sha"] == sha)
            .unwrap_or_else(|| panic!("commit {sha} missing from discarded list: {discarded:?}"))
            ["survival"]
            .as_str()
            .expect("survival must be a string")
            .to_string()
    };
    for (label, sha) in [("C1", &c1_sha), ("C2", &c2_sha), ("C3", &c3_sha)] {
        assert_eq!(
            verdict_for(sha),
            "already-in-main-by-cumulative-diff",
            "{label} ({sha}) should have survived via the cumulative fallback, not been left \
             an orphan -- discarded: {discarded:?}"
        );
    }
    assert_eq!(
        verdict_for(&old_remote_sha),
        "present-in-new-history",
        "topic's own commit should still survive via the ordinary per-commit patch-id test"
    );
}

/// Done-When #2: a branch with a genuinely orphaned remote commit is
/// refused, names that commit, and proceeds only with `--force-reason`.
#[cfg(unix)]
#[test]
fn push_force_refuses_orphan_commit_names_it_and_succeeds_with_reason() {
    let _guard = RealRepoConfigGuard::new();
    let remote = init_bare_remote();
    let local = setup_local_repo(remote.path()); // leaves checkout on feat/x, pushed
    let lp = local.path();
    run_git_fixture(lp, &["push", "-q", "origin", "feat/x"]);

    let orphan_sha = run_git_fixture_output(lp, &["rev-parse", "HEAD"]);
    let orphan_subject = run_git_fixture_output(lp, &["log", "-1", "--format=%s"]);

    // Drop the commit locally with no replacement anywhere -- a genuine
    // orphan: not present in the new history, not on main by sha, not on
    // main by patch-id.
    run_git_fixture(lp, &["reset", "-q", "--hard", "main"]);
    let new_local_sha = run_git_fixture_output(lp, &["rev-parse", "HEAD"]);

    let data_dir = tempfile::tempdir().unwrap();
    let (_stdout, stderr) = run_fail(push_cmd(data_dir.path(), lp).args([
        "push",
        "--repo",
        "test-agent",
        "--branch",
        "feat/x",
        "--force",
    ]));
    assert!(
        stderr.contains(&format!("{orphan_sha} {orphan_subject}")),
        "expected the orphan named as '<sha> <subject>', got: {stderr}"
    );
    assert!(
        stderr.contains("--force-reason"),
        "expected the override named, got: {stderr}"
    );

    let remote_after_refusal = rev_parse(remote.path(), "refs/heads/feat/x");
    assert_eq!(
        String::from_utf8_lossy(&remote_after_refusal.stdout).trim(),
        orphan_sha,
        "the refused push must not have touched the remote"
    );

    let stdout = run_ok(push_cmd(data_dir.path(), lp).args([
        "push",
        "--repo",
        "test-agent",
        "--branch",
        "feat/x",
        "--force",
        "--force-reason",
        "confirmed the dropped commit is intentionally gone",
    ]));
    assert!(stdout.contains("feat/x"), "got: {stdout}");

    let remote_after_override = rev_parse(remote.path(), "refs/heads/feat/x");
    assert_eq!(
        String::from_utf8_lossy(&remote_after_override.stdout).trim(),
        new_local_sha,
        "expected the override push to have updated the remote"
    );

    let details = audit_details_for(data_dir.path(), "feat/x");
    assert_eq!(
        details.len(),
        2,
        "expected the refused attempt AND the overridden attempt both audited: {details:?}"
    );
    let refused = &details[0];
    let overridden = &details[1];

    assert_eq!(refused["outcome"], "failure", "{refused:?}");
    assert_eq!(overridden["outcome"], "success", "{overridden:?}");
    assert_eq!(refused["discarded_count"], 1, "{refused:?}");
    assert_eq!(refused["discarded"][0]["sha"], orphan_sha, "{refused:?}");
    assert_eq!(
        refused["discarded"][0]["survival"], "orphan",
        "the refused attempt's audit row must record the orphan verdict: {refused:?}"
    );
    assert!(
        refused["force_reason"].is_null(),
        "no reason was given on the refused attempt: {refused:?}"
    );

    assert_eq!(
        overridden["discarded"][0]["survival"], "orphan",
        "the overridden attempt's audit row must ALSO record the orphan verdict -- the \
         override changes the outcome, not the analysis: {overridden:?}"
    );
    assert_eq!(
        overridden["force_reason"], "confirmed the dropped commit is intentionally gone",
        "{overridden:?}"
    );
}

/// Done-When #4: `main` is refused with `--force` exactly as without it --
/// `validate_branch` runs before the force analysis ever starts.
#[cfg(unix)]
#[test]
fn push_force_still_refuses_main() {
    let remote = init_bare_remote();
    let local = setup_local_repo(remote.path());
    let data_dir = tempfile::tempdir().unwrap();

    let (_stdout, stderr) = run_fail(push_cmd(data_dir.path(), local.path()).args([
        "push",
        "--repo",
        "test-agent",
        "--branch",
        "main",
        "--force",
    ]));
    assert!(
        stderr.contains("main") && stderr.to_lowercase().contains("refus"),
        "expected a refusal naming main, got: {stderr}"
    );
}

/// `--force` and `--tag` are mutually exclusive at the clap layer -- a moved
/// tag is a different, out-of-scope problem.
#[cfg(unix)]
#[test]
fn push_force_conflicts_with_tag() {
    let data_dir = tempfile::tempdir().unwrap();
    let local = tempfile::tempdir().unwrap();

    let (_stdout, stderr) = run_fail(push_cmd(data_dir.path(), local.path()).args([
        "push",
        "--repo",
        "test-agent",
        "--tag",
        "v1.0.0",
        "--force",
    ]));
    assert!(
        stderr.contains("force") && stderr.to_lowercase().contains("cannot be used"),
        "expected clap's conflicts_with refusal, got: {stderr}"
    );
}
