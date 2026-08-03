//! Integration tests for `legion commit` (#854): the audited commit verb.
//!
//! Every test drives a real `git` fixture repo, because the command's whole
//! job is shelling out to git -- resolving the checkout, probing the signer,
//! and running the commit.
//!
//! Two isolation rules these fixtures must follow, and neither is optional:
//!
//! 1. **Identity goes in the temp repo's LOCAL config**, not as `-c`
//!    overrides. `run_git_fixture` passes identity per-invocation and never
//!    writes it (#723), which is right for fixture setup -- but the binary
//!    under test spawns its own `git commit` with no `-c` flags, so without
//!    a local config those subprocesses fail with "Committer identity
//!    unknown" and every commit test fails for the wrong reason. Writing to
//!    a tempdir's own config is safe; `RealRepoConfigGuard` protects the
//!    enclosing real checkout, which is untouched here.
//! 2. **`GIT_CONFIG_GLOBAL`/`GIT_CONFIG_SYSTEM` go on the binary's env.**
//!    The operator's real global config carries `commit.gpgsign=true` and a
//!    `core.hooksPath` pointing at this repo's pre-commit hook, which runs a
//!    nested Claude review with a two-minute budget. Isolating keeps these
//!    tests hermetic and fast.

use crate::common::*;
use std::path::Path;

/// The message every happy-path test commits: house style, so a failure
/// here means the verb is wrong rather than the fixture.
const GOOD_MESSAGE: &str = "feat(#854): add a thing\n\
                            \n\
                            Body paragraph explaining why.\n\
                            \n\
                            Co-Authored-By: Legion Test <fixture@example.invalid>\n";

/// A repo with one commit on `main`, a second change already staged, and
/// identity plus `commit.gpgsign=false` written into its LOCAL config so
/// the binary's own `git` subprocesses inherit them. Signing-off by config
/// is what keeps CI from needing a signer; the preflight path gets its own
/// test that turns signing back on.
fn setup_repo_with_staged_change() -> tempfile::TempDir {
    let repo = tempfile::tempdir().unwrap();
    let rp = repo.path();
    run_git_fixture(rp, &["init", "-q", "-b", "main"]);
    run_git_fixture(rp, &["config", "user.name", "Legion Test Fixture"]);
    run_git_fixture(rp, &["config", "user.email", "fixture@example.invalid"]);
    run_git_fixture(rp, &["config", "commit.gpgsign", "false"]);

    std::fs::write(rp.join("README.md"), "seed\n").unwrap();
    run_git_fixture(rp, &["add", "README.md"]);
    run_git_fixture(rp, &["commit", "-q", "-m", "seed"]);

    std::fs::write(rp.join("feature.txt"), "change\n").unwrap();
    run_git_fixture(rp, &["add", "feature.txt"]);

    repo
}

/// `legion` invoked in `cwd` with the host's global/system git config
/// swapped out. See the module docs for why this is on the command under
/// test and not just on fixture setup.
fn commit_cmd(data_dir: &Path, cwd: &Path) -> std::process::Command {
    let (global, system) = isolated_git_config_paths();
    let mut cmd = legion_cmd(data_dir);
    cmd.current_dir(cwd)
        .env("GIT_CONFIG_GLOBAL", global)
        .env("GIT_CONFIG_SYSTEM", system);
    cmd
}

fn head_sha(repo: &Path) -> String {
    run_git_fixture_output(repo, &["rev-parse", "HEAD"])
}

fn head_message(repo: &Path) -> String {
    run_git_fixture_output(repo, &["log", "-1", "--format=%B"])
}

/// Run `legion commit --message <msg>` and expect a refusal, returning
/// stderr. Every convention test is this shape.
fn refuse_message(repo: &Path, data_dir: &Path, message: &str) -> String {
    let (_stdout, stderr) = run_fail(commit_cmd(data_dir, repo).args([
        "commit",
        "--repo",
        "test-agent",
        "--message",
        message,
    ]));
    stderr
}

/// Happy path: the staged change lands, the message survives verbatim, and
/// the confirmation names the branch.
#[cfg(unix)]
#[test]
fn commit_lands_the_staged_change_and_reports_it() {
    let _guard = RealRepoConfigGuard::new();
    let repo = setup_repo_with_staged_change();
    let data_dir = tempfile::tempdir().unwrap();
    let before = head_sha(repo.path());

    let stdout = run_ok(commit_cmd(data_dir.path(), repo.path()).args([
        "commit",
        "--repo",
        "test-agent",
        "--message",
        GOOD_MESSAGE,
    ]));

    let after = head_sha(repo.path());
    assert_ne!(before, after, "expected HEAD to advance");
    assert!(
        stdout.contains("main"),
        "expected the confirmation to name the branch, got: {stdout}"
    );
    assert!(
        stdout.contains(&after[..8]),
        "expected the confirmation to name the new commit, got: {stdout}"
    );

    let body = head_message(repo.path());
    assert!(body.contains("feat(#854): add a thing"), "got: {body}");
    assert!(
        body.contains("Body paragraph explaining why."),
        "the body must survive --cleanup=whitespace, got: {body}"
    );
    assert!(body.contains("Co-Authored-By:"), "got: {body}");
}

/// Only the staged index is committed. `legion commit` never stages for
/// you, so an unstaged file must still be unstaged afterwards.
#[cfg(unix)]
#[test]
fn commit_leaves_unstaged_changes_alone() {
    let _guard = RealRepoConfigGuard::new();
    let repo = setup_repo_with_staged_change();
    std::fs::write(repo.path().join("untracked.txt"), "not staged\n").unwrap();
    let data_dir = tempfile::tempdir().unwrap();

    run_ok(commit_cmd(data_dir.path(), repo.path()).args([
        "commit",
        "--repo",
        "test-agent",
        "--message",
        GOOD_MESSAGE,
    ]));

    let status = run_git_fixture_output(repo.path(), &["status", "--porcelain"]);
    assert!(
        status.contains("untracked.txt"),
        "the untracked file must be untouched, got: {status:?}"
    );
    let committed =
        run_git_fixture_output(repo.path(), &["show", "--name-only", "--format=", "HEAD"]);
    assert!(committed.contains("feature.txt"), "got: {committed}");
    assert!(!committed.contains("untracked.txt"), "got: {committed}");
}

/// Works from a subdirectory: the checkout is the repo root, not the CWD.
#[cfg(unix)]
#[test]
fn commit_resolves_the_repo_root_from_a_subdirectory() {
    let _guard = RealRepoConfigGuard::new();
    let repo = setup_repo_with_staged_change();
    let sub = repo.path().join("nested/deeper");
    std::fs::create_dir_all(&sub).unwrap();
    let data_dir = tempfile::tempdir().unwrap();

    run_ok(commit_cmd(data_dir.path(), &sub).args([
        "commit",
        "--repo",
        "test-agent",
        "--message",
        GOOD_MESSAGE,
    ]));

    assert!(head_message(repo.path()).contains("feat(#854): add a thing"));
}

/// A subject with no scope is refused by name. Every conventional subject
/// in this repo's recent history carries one.
#[cfg(unix)]
#[test]
fn commit_refuses_unscoped_subject() {
    let _guard = RealRepoConfigGuard::new();
    let repo = setup_repo_with_staged_change();
    let data_dir = tempfile::tempdir().unwrap();
    let before = head_sha(repo.path());

    let stderr = refuse_message(
        repo.path(),
        data_dir.path(),
        "feat: no scope\n\nCo-Authored-By: Legion Test <fixture@example.invalid>\n",
    );
    assert!(stderr.contains("bad subject line"), "got: {stderr}");
    assert!(stderr.contains("<scope>"), "got: {stderr}");
    assert_eq!(before, head_sha(repo.path()), "a refusal must not commit");
}

/// An unknown commit type is refused, naming the type.
#[cfg(unix)]
#[test]
fn commit_refuses_unknown_type() {
    let _guard = RealRepoConfigGuard::new();
    let repo = setup_repo_with_staged_change();
    let data_dir = tempfile::tempdir().unwrap();

    let stderr = refuse_message(
        repo.path(),
        data_dir.path(),
        "wip(#854): halfway\n\nCo-Authored-By: Legion Test <fixture@example.invalid>\n",
    );
    assert!(stderr.contains("unknown type 'wip'"), "got: {stderr}");
}

/// A message with no `Co-Authored-By` trailer is refused by name.
#[cfg(unix)]
#[test]
fn commit_refuses_missing_coauthor_trailer() {
    let _guard = RealRepoConfigGuard::new();
    let repo = setup_repo_with_staged_change();
    let data_dir = tempfile::tempdir().unwrap();
    let before = head_sha(repo.path());

    let stderr = refuse_message(
        repo.path(),
        data_dir.path(),
        "feat(#854): add a thing\n\nBody with no trailer.\n",
    );
    assert!(stderr.contains("Co-Authored-By"), "got: {stderr}");
    assert_eq!(before, head_sha(repo.path()), "a refusal must not commit");
}

/// An emoji anywhere in the message is refused, naming the codepoint.
#[cfg(unix)]
#[test]
fn commit_refuses_emoji() {
    let _guard = RealRepoConfigGuard::new();
    let repo = setup_repo_with_staged_change();
    let data_dir = tempfile::tempdir().unwrap();
    let before = head_sha(repo.path());

    let stderr = refuse_message(
        repo.path(),
        data_dir.path(),
        "feat(#854): add a thing\n\nShipped \u{1F680}\n\nCo-Authored-By: L <f@example.invalid>\n",
    );
    assert!(stderr.contains("emoji"), "got: {stderr}");
    assert!(stderr.contains("U+1F680"), "got: {stderr}");
    assert_eq!(before, head_sha(repo.path()), "a refusal must not commit");
}

/// `--message` and `--message-file` together is a refusal, not a silent
/// precedence rule.
#[cfg(unix)]
#[test]
fn commit_refuses_both_message_sources() {
    let _guard = RealRepoConfigGuard::new();
    let repo = setup_repo_with_staged_change();
    let data_dir = tempfile::tempdir().unwrap();
    let msg_file = repo.path().join("msg.txt");
    std::fs::write(&msg_file, GOOD_MESSAGE).unwrap();

    let (_stdout, stderr) = run_fail(commit_cmd(data_dir.path(), repo.path()).args([
        "commit",
        "--repo",
        "test-agent",
        "--message",
        GOOD_MESSAGE,
        "--message-file",
        msg_file.to_str().unwrap(),
    ]));
    assert!(stderr.contains("mutually exclusive"), "got: {stderr}");
}

/// `--message-file` is the path the bootstrap commit uses, so it gets its
/// own happy-path coverage rather than riding on `--message`.
#[cfg(unix)]
#[test]
fn commit_accepts_a_message_file() {
    let _guard = RealRepoConfigGuard::new();
    let repo = setup_repo_with_staged_change();
    let data_dir = tempfile::tempdir().unwrap();
    // Outside the repo, so the message file is not itself a staged change.
    let msg_dir = tempfile::tempdir().unwrap();
    let msg_file = msg_dir.path().join("msg.txt");
    std::fs::write(&msg_file, GOOD_MESSAGE).unwrap();

    run_ok(commit_cmd(data_dir.path(), repo.path()).args([
        "commit",
        "--repo",
        "test-agent",
        "--message-file",
        msg_file.to_str().unwrap(),
    ]));
    assert!(head_message(repo.path()).contains("feat(#854): add a thing"));
}

/// Outside a git repo the verb refuses rather than emitting a raw git
/// error the caller has to decode.
#[cfg(unix)]
#[test]
fn commit_refuses_outside_a_git_repository() {
    let not_a_repo = tempfile::tempdir().unwrap();
    let data_dir = tempfile::tempdir().unwrap();

    let (_stdout, stderr) = run_fail(commit_cmd(data_dir.path(), not_a_repo.path()).args([
        "commit",
        "--repo",
        "test-agent",
        "--message",
        GOOD_MESSAGE,
    ]));
    assert!(
        stderr.contains("not inside a git repository"),
        "got: {stderr}"
    );
}

/// The preflight is the point of the verb: with signing enabled and the
/// signer pointed at a program that always fails, the commit is refused by
/// name BEFORE anything is written, naming the program to unlock.
///
/// `/usr/bin/false` emits nothing at all, so this also exercises the
/// empty-signer-output fallback -- the assertion is on the refusal's shape
/// and the program name, never on text `/usr/bin/false` did not produce.
#[cfg(unix)]
#[test]
fn commit_preflight_refuses_when_the_signer_cannot_sign() {
    let _guard = RealRepoConfigGuard::new();
    let repo = setup_repo_with_staged_change();
    let rp = repo.path();
    run_git_fixture(rp, &["config", "commit.gpgsign", "true"]);
    run_git_fixture(rp, &["config", "gpg.format", "ssh"]);
    run_git_fixture(rp, &["config", "gpg.ssh.program", "/usr/bin/false"]);
    run_git_fixture(
        rp,
        &[
            "config",
            "user.signingkey",
            "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIExampleKeyForFixtureUseOnly000000000",
        ],
    );
    let data_dir = tempfile::tempdir().unwrap();
    let before = head_sha(rp);

    let (_stdout, stderr) = run_fail(commit_cmd(data_dir.path(), rp).args([
        "commit",
        "--repo",
        "test-agent",
        "--message",
        GOOD_MESSAGE,
    ]));

    assert!(stderr.contains("signing unavailable"), "got: {stderr}");
    assert!(
        stderr.contains("/usr/bin/false"),
        "the refusal must name the program to unlock, got: {stderr}"
    );
    assert!(stderr.contains("unlock your signer"), "got: {stderr}");
    assert_eq!(
        before,
        head_sha(rp),
        "the preflight must refuse before anything is committed"
    );
}

/// A locked signer is caught by the preflight even when the message is
/// ALSO invalid: preflight runs first on purpose, because a locked signer
/// needs the operator to go unlock something while a bad subject is a
/// five-second fix, and surfacing the slow failure first means one round
/// trip instead of two.
#[cfg(unix)]
#[test]
fn commit_preflight_runs_before_message_validation() {
    let _guard = RealRepoConfigGuard::new();
    let repo = setup_repo_with_staged_change();
    let rp = repo.path();
    run_git_fixture(rp, &["config", "commit.gpgsign", "true"]);
    run_git_fixture(rp, &["config", "gpg.format", "ssh"]);
    run_git_fixture(rp, &["config", "gpg.ssh.program", "/usr/bin/false"]);
    run_git_fixture(
        rp,
        &[
            "config",
            "user.signingkey",
            "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIExampleKeyForFixtureUseOnly000000000",
        ],
    );
    let data_dir = tempfile::tempdir().unwrap();

    let stderr = refuse_message(rp, data_dir.path(), "garbage subject, no trailer");
    assert!(stderr.contains("signing unavailable"), "got: {stderr}");
}

/// The audit row is the reason this verb exists. A successful commit
/// records the resolved checkout, both SHAs, the card, and the gate state
/// of the commit it was built on.
#[cfg(unix)]
#[test]
fn commit_writes_audit_row_with_shas_card_and_gates() {
    let _guard = RealRepoConfigGuard::new();
    let repo = setup_repo_with_staged_change();
    let data_dir = tempfile::tempdir().unwrap();
    let before = head_sha(repo.path());

    run_ok(commit_cmd(data_dir.path(), repo.path()).args([
        "commit",
        "--repo",
        "test-agent",
        "--message",
        GOOD_MESSAGE,
        "--card",
        "card-854",
    ]));
    let after = head_sha(repo.path());

    let audit_out =
        run_ok(legion_cmd(data_dir.path()).args(["audit", "--action", "commit", "--json"]));
    assert!(
        audit_out.contains("\"action\": \"commit\""),
        "got: {audit_out}"
    );
    assert!(
        audit_out.contains("\"outcome\": \"success\""),
        "got: {audit_out}"
    );
    assert!(
        audit_out.contains("\"target_ref\": \"main\""),
        "the audit row keys on the branch, got: {audit_out}"
    );
    assert!(
        audit_out.contains(repo.path().to_str().unwrap()),
        "expected the resolved checkout in the details, got: {audit_out}"
    );
    assert!(
        audit_out.contains(&before),
        "expected the pre-commit SHA in the details, got: {audit_out}"
    );
    assert!(
        audit_out.contains(&after),
        "expected the post-commit SHA in the details, got: {audit_out}"
    );
    assert!(audit_out.contains("card-854"), "got: {audit_out}");
    // No gates were recorded on the pre-commit HEAD, which is a real state
    // ("absent"), distinct from a gate that ran and found issues.
    assert!(
        audit_out.contains("\\\"simplify\\\":\\\"absent\\\""),
        "expected the simplify gate verdict in the details, got: {audit_out}"
    );
    assert!(
        audit_out.contains("\\\"pr_write\\\":\\\"absent\\\""),
        "expected the pr-write gate verdict in the details, got: {audit_out}"
    );
}

/// A refused commit is still an attempted mutation, and the audit trail
/// exists to carry exactly that -- especially once the deferred PreToolUse
/// hook starts routing every plain `git commit` through this verb.
#[cfg(unix)]
#[test]
fn commit_writes_a_failure_audit_row_when_it_refuses() {
    let _guard = RealRepoConfigGuard::new();
    let repo = setup_repo_with_staged_change();
    let data_dir = tempfile::tempdir().unwrap();

    refuse_message(
        repo.path(),
        data_dir.path(),
        "feat(#854): add a thing\n\nBody with no trailer.\n",
    );

    let audit_out =
        run_ok(legion_cmd(data_dir.path()).args(["audit", "--action", "commit", "--json"]));
    assert!(
        audit_out.contains("\"outcome\": \"failure\""),
        "expected a failure-outcome row for the refused commit, got: {audit_out}"
    );
    assert!(
        audit_out.contains("\\\"post_sha\\\":null"),
        "a refused commit has no post SHA, got: {audit_out}"
    );
}

/// An underlying `git commit` failure (nothing staged) surfaces git's own
/// text and is audited as a failure -- the message passed validation, so
/// this exercises the run-git-commit arm rather than a refusal.
#[cfg(unix)]
#[test]
fn commit_underlying_git_failure_surfaces_and_audits() {
    let _guard = RealRepoConfigGuard::new();
    let repo = tempfile::tempdir().unwrap();
    let rp = repo.path();
    run_git_fixture(rp, &["init", "-q", "-b", "main"]);
    run_git_fixture(rp, &["config", "user.name", "Legion Test Fixture"]);
    run_git_fixture(rp, &["config", "user.email", "fixture@example.invalid"]);
    run_git_fixture(rp, &["config", "commit.gpgsign", "false"]);
    std::fs::write(rp.join("README.md"), "seed\n").unwrap();
    run_git_fixture(rp, &["add", "README.md"]);
    run_git_fixture(rp, &["commit", "-q", "-m", "seed"]);
    // Nothing staged beyond the seed commit.

    let data_dir = tempfile::tempdir().unwrap();
    let (_stdout, stderr) = run_fail(commit_cmd(data_dir.path(), rp).args([
        "commit",
        "--repo",
        "test-agent",
        "--message",
        GOOD_MESSAGE,
    ]));
    assert!(
        stderr.contains("git commit failed"),
        "expected the commit-failed error, got: {stderr}"
    );

    let audit_out =
        run_ok(legion_cmd(data_dir.path()).args(["audit", "--action", "commit", "--json"]));
    assert!(
        audit_out.contains("\"outcome\": \"failure\""),
        "got: {audit_out}"
    );
}

/// The preflight must fire for every spelling git accepts as true, not just
/// the literal `true`. `commit.gpgsign = yes` is valid git config; reading
/// it as a raw string makes it read as "off", skips the preflight, and then
/// signs for real during the commit -- surfacing the exact cryptic failure
/// the preflight exists to replace, with nothing erroring to say why.
#[cfg(unix)]
#[test]
fn commit_preflight_fires_for_every_truthy_gpgsign_spelling() {
    let _guard = RealRepoConfigGuard::new();
    for spelling in ["yes", "on", "1", "True"] {
        let repo = setup_repo_with_staged_change();
        let rp = repo.path();
        run_git_fixture(rp, &["config", "commit.gpgsign", spelling]);
        run_git_fixture(rp, &["config", "gpg.format", "ssh"]);
        run_git_fixture(rp, &["config", "gpg.ssh.program", "/usr/bin/false"]);
        run_git_fixture(
            rp,
            &[
                "config",
                "user.signingkey",
                "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIExampleKeyForFixtureUseOnly000000000",
            ],
        );
        let data_dir = tempfile::tempdir().unwrap();
        let before = head_sha(rp);

        let (_stdout, stderr) = run_fail(commit_cmd(data_dir.path(), rp).args([
            "commit",
            "--repo",
            "test-agent",
            "--message",
            GOOD_MESSAGE,
        ]));

        assert!(
            stderr.contains("signing unavailable"),
            "commit.gpgsign={spelling} must preflight, got: {stderr}"
        );
        assert_eq!(
            before,
            head_sha(rp),
            "commit.gpgsign={spelling} must refuse before committing"
        );
    }
}
