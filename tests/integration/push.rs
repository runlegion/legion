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
/// refused -- this command has no `--force` flag, and a crafted branch
/// value must not be able to recover force semantics.
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
