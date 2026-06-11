//! Integration tests: worksource plugin protocol, pr read surface, quality gates, pr create, sync.

use crate::common::*;
use std::process::Command;

/// Verify `legion pr close` fails with a clear error when the repo has no
/// work source config in watch.toml. Network access is not available in tests,
/// so we confirm the CLI is correctly wired without invoking `gh`.
#[test]
fn pr_close_errors_without_worksource_config() {
    let dir = tempfile::tempdir().unwrap();

    let (_stdout, stderr) = run_fail(legion_cmd(dir.path()).args([
        "pr",
        "close",
        "--repo",
        "no-such-repo",
        "--number",
        "42",
    ]));
    assert!(
        stderr.contains("no work source configured"),
        "expected 'no work source configured' in stderr, got: {stderr}"
    );
}

/// Verify `legion pr close --delete-branch` flag is accepted by the CLI parser.
/// Errors at the worksource level (no config) rather than at argument parsing.
#[test]
fn pr_close_delete_branch_flag_accepted() {
    let dir = tempfile::tempdir().unwrap();

    // Fails at worksource resolution, not arg parsing -- confirms all flags parse
    let (_stdout, stderr) = run_fail(legion_cmd(dir.path()).args([
        "pr",
        "close",
        "--repo",
        "no-such-repo",
        "--number",
        "42",
        "--reason",
        "superseded",
        "--delete-branch",
    ]));
    assert!(
        stderr.contains("no work source configured"),
        "expected worksource error, got: {stderr}"
    );
}

#[test]
fn pr_checks_errors_without_worksource_config() {
    let dir = tempfile::tempdir().unwrap();

    let (_stdout, stderr) = run_fail(legion_cmd(dir.path()).args([
        "pr",
        "checks",
        "--repo",
        "no-such-repo",
        "--number",
        "42",
    ]));
    assert!(
        stderr.contains("no work source configured"),
        "expected 'no work source configured' in stderr, got: {stderr}"
    );
}

#[test]
fn pr_checks_json_flag_accepted() {
    let dir = tempfile::tempdir().unwrap();

    // Fails at worksource resolution, not arg parsing -- confirms --json parses
    let (_stdout, stderr) = run_fail(legion_cmd(dir.path()).args([
        "pr",
        "checks",
        "--repo",
        "no-such-repo",
        "--number",
        "42",
        "--json",
    ]));
    assert!(
        stderr.contains("no work source configured"),
        "expected worksource error, got: {stderr}"
    );
}

// ---------------------------------------------------------------------------
// PR read-side (view / comments / reviews / checks --log-failed) with a
// stubbed worksource plugin. The plugin is a bash script that dispatches on
// the subcommand, ignores the env vars, and echoes the fixture contents.
//
// Gated on #[cfg(unix)] because the stub relies on a bash interpreter, an
// exec bit (chmod 0o755 via PermissionsExt), and the plugin-resolution path
// that legion uses via `Command::new(plugin_path)`. Windows CI builds and
// tests everything else; the worksource plugin itself is bash-based and the
// read surface is exercised end-to-end on ubuntu/macos runners.
// ---------------------------------------------------------------------------

#[cfg(unix)]
fn pr_read_stub_plugin(view_pr: &str, comments: &str, reviews: &str, check_log: &str) -> String {
    // Encode each fixture as a single-quoted heredoc body so shell escaping
    // inside the fixture content (backticks, dollars, braces) cannot break
    // the dispatch script. Uses `cat <<'BODY'` which is literal.
    format!(
        r#"#!/bin/bash
set -e
case "${{1:-}}" in
  view-pr)
    cat <<'BODY'
{view_pr}
BODY
    ;;
  pr-comments)
    cat <<'BODY'
{comments}
BODY
    ;;
  pr-reviews)
    cat <<'BODY'
{reviews}
BODY
    ;;
  pr-checks)
    # Minimal fixture matching ExternalPRCheck shape so `pr checks --log-failed`
    # has something to iterate. One failing check, one success, plus one
    # failure whose link does not match the Actions pattern so the
    # non-Actions-link branch runs.
    cat <<'BODY'
[
  {{"name":"Clippy","state":"FAILURE","workflow":"CI","link":"https://github.com/ex/ex/actions/runs/1/job/42","description":""}},
  {{"name":"Tests","state":"SUCCESS","workflow":"CI","link":"https://github.com/ex/ex/actions/runs/1/job/99","description":""}},
  {{"name":"External","state":"FAILURE","workflow":"CI","link":"https://dashboard.example.com/checks/abc","description":""}}
]
BODY
    ;;
  pr-check-log)
    cat <<'BODY'
{check_log}
BODY
    ;;
  *)
    echo "stub: unknown subcommand $1" >&2
    exit 2
    ;;
esac
"#
    )
}

#[cfg(unix)]
fn setup_pr_read_stub(
    data_dir: &std::path::Path,
    plugin_root: &std::path::Path,
    body: &str,
) -> std::path::PathBuf {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    let worksources = plugin_root.join("worksources");
    fs::create_dir_all(&worksources).unwrap();
    let plugin_path = worksources.join("github");
    fs::write(&plugin_path, body).unwrap();
    let mut perm = fs::metadata(&plugin_path).unwrap().permissions();
    perm.set_mode(0o755);
    fs::set_permissions(&plugin_path, perm).unwrap();

    // watch.toml pointing a fake repo at the stub plugin.
    let watch = format!(
        r#"poll_interval_secs = 30
cooldown_secs = 300

[[repos]]
name = "stub"
github = "owner/stub"
workdir = "{}"
worksource = "github"
"#,
        data_dir.display()
    );
    fs::write(data_dir.join("watch.toml"), watch).unwrap();

    plugin_path
}

#[cfg(unix)]
fn pr_read_cmd(data_dir: &std::path::Path, plugin_root: &std::path::Path) -> Command {
    let mut cmd = legion_cmd(data_dir);
    cmd.env("CLAUDE_PLUGIN_ROOT", plugin_root);
    cmd
}

#[cfg(unix)]
#[test]
fn pr_view_renders_body_and_metadata() {
    let data_dir = tempfile::tempdir().unwrap();
    let plugin_root = tempfile::tempdir().unwrap();
    let body = pr_read_stub_plugin(
        r#"{
  "number": 42,
  "title": "stub PR",
  "state": "OPEN",
  "author": "alice",
  "createdAt": "2026-04-21T10:00:00Z",
  "updatedAt": "2026-04-21T11:00:00Z",
  "body": "multi-line\nbody",
  "headRefName": "feat/x",
  "headSha": "deadbeef",
  "baseRefName": "main",
  "isDraft": false,
  "reviewDecision": "REVIEW_REQUIRED",
  "mergeable": "MERGEABLE"
}"#,
        "[]",
        "[]",
        "",
    );
    setup_pr_read_stub(data_dir.path(), plugin_root.path(), &body);

    let stdout = run_ok(
        pr_read_cmd(data_dir.path(), plugin_root.path())
            .args(["pr", "view", "--repo", "stub", "--number", "42"]),
    );
    assert!(stdout.contains("PR #42"));
    assert!(stdout.contains("stub PR"));
    assert!(stdout.contains("OPEN"));
    assert!(stdout.contains("feat/x -> main"));
    assert!(stdout.contains("REVIEW_REQUIRED"));
    assert!(stdout.contains("multi-line"));
}

#[cfg(unix)]
#[test]
fn pr_view_json_roundtrips() {
    let data_dir = tempfile::tempdir().unwrap();
    let plugin_root = tempfile::tempdir().unwrap();
    let body = pr_read_stub_plugin(
        r#"{
  "number": 1,
  "title": "t",
  "state": "MERGED",
  "author": "a",
  "createdAt": "2026-04-21T00:00:00Z",
  "updatedAt": "2026-04-21T00:00:00Z",
  "body": "x",
  "headRefName": "f",
  "headSha": "0",
  "baseRefName": "main",
  "isDraft": false,
  "reviewDecision": null,
  "mergeable": null
}"#,
        "[]",
        "[]",
        "",
    );
    setup_pr_read_stub(data_dir.path(), plugin_root.path(), &body);

    let stdout = run_ok(
        pr_read_cmd(data_dir.path(), plugin_root.path())
            .args(["pr", "view", "--repo", "stub", "--number", "1", "--json"]),
    );
    let parsed: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON");
    assert_eq!(parsed["number"], 1);
    assert_eq!(parsed["state"], "MERGED");
}

#[cfg(unix)]
#[test]
fn pr_comments_handles_empty_thread() {
    let data_dir = tempfile::tempdir().unwrap();
    let plugin_root = tempfile::tempdir().unwrap();
    let body = pr_read_stub_plugin("{}", "[]", "[]", "");
    setup_pr_read_stub(data_dir.path(), plugin_root.path(), &body);

    let stderr = run_ok_stderr(
        pr_read_cmd(data_dir.path(), plugin_root.path())
            .args(["pr", "comments", "--repo", "stub", "--number", "1"]),
    );
    assert!(
        stderr.contains("no comments"),
        "expected empty-thread notice, got stderr: {stderr}"
    );
}

#[cfg(unix)]
#[test]
fn pr_comments_renders_issue_and_review_mix() {
    let data_dir = tempfile::tempdir().unwrap();
    let plugin_root = tempfile::tempdir().unwrap();
    let body = pr_read_stub_plugin(
        "{}",
        r#"[
  {"id":"1","author":"alice","createdAt":"2026-04-21T09:00:00Z","updatedAt":"2026-04-21T09:00:00Z","body":"top-level thoughts","kind":"issue","path":null,"line":null,"inReplyToId":null},
  {"id":"2","author":"bob","createdAt":"2026-04-21T09:30:00Z","updatedAt":"2026-04-21T09:30:00Z","body":"fix this line","kind":"review","path":"src/foo.rs","line":42,"inReplyToId":null}
]"#,
        "[]",
        "",
    );
    setup_pr_read_stub(data_dir.path(), plugin_root.path(), &body);

    let stdout = run_ok(
        pr_read_cmd(data_dir.path(), plugin_root.path())
            .args(["pr", "comments", "--repo", "stub", "--number", "1"]),
    );
    assert!(stdout.contains("[issue]"));
    assert!(stdout.contains("top-level thoughts"));
    assert!(stdout.contains("[review] src/foo.rs:42"));
    assert!(stdout.contains("fix this line"));
}

#[cfg(unix)]
#[test]
fn pr_reviews_renders_inline_comments_grouped() {
    let data_dir = tempfile::tempdir().unwrap();
    let plugin_root = tempfile::tempdir().unwrap();
    let body = pr_read_stub_plugin(
        "{}",
        "[]",
        r#"[
  {
    "id":"10",
    "author":"vault",
    "state":"CHANGES_REQUESTED",
    "submittedAt":"2026-04-21T10:00:00Z",
    "body":"needs work",
    "comments":[
      {"id":"20","author":"vault","createdAt":"2026-04-21T10:00:00Z","updatedAt":"2026-04-21T10:00:00Z","body":"inline nit","kind":"review","path":"src/x.rs","line":12,"inReplyToId":null}
    ]
  }
]"#,
        "",
    );
    setup_pr_read_stub(data_dir.path(), plugin_root.path(), &body);

    let stdout = run_ok(
        pr_read_cmd(data_dir.path(), plugin_root.path())
            .args(["pr", "reviews", "--repo", "stub", "--number", "1"]),
    );
    assert!(stdout.contains("[CHANGES_REQUESTED] vault"));
    assert!(stdout.contains("needs work"));
    assert!(stdout.contains("[review] src/x.rs:12"));
    assert!(stdout.contains("inline nit"));
}

#[cfg(unix)]
#[test]
fn pr_checks_log_failed_streams_failing_job_logs() {
    let data_dir = tempfile::tempdir().unwrap();
    let plugin_root = tempfile::tempdir().unwrap();
    let body = pr_read_stub_plugin(
        "{}",
        "[]",
        "[]",
        "error[E0308]: mismatched types\n  --> src/lib.rs:3:5",
    );
    setup_pr_read_stub(data_dir.path(), plugin_root.path(), &body);

    // Exits non-zero because the stub returns a FAILURE check.
    let (stdout, _stderr) = run_fail(pr_read_cmd(data_dir.path(), plugin_root.path()).args([
        "pr",
        "checks",
        "--repo",
        "stub",
        "--number",
        "1",
        "--log-failed",
    ]));
    // Actions-linked failure: job id extracted from link, header + log emitted.
    assert!(stdout.contains("===== Clippy (42) ====="));
    assert!(stdout.contains("error[E0308]"));
    // Non-Actions link: skipped with a marker rather than crashing or
    // silently swallowing the failure.
    assert!(stdout.contains("===== External ====="));
    assert!(stdout.contains("non-Actions check link"));
}

/// A stub where pr-check-log always exits non-zero. Exercises the Err branch
/// of fetch_check_log inside the --log-failed loop -- partial failures must
/// render a marker and the outer run must still exit non-zero on the failing
/// check, independently of whether the log fetch succeeded.
#[cfg(unix)]
fn pr_read_stub_plugin_failing_log() -> String {
    r#"#!/bin/bash
set -e
case "${1:-}" in
  pr-checks)
    cat <<'BODY'
[{"name":"Clippy","state":"FAILURE","workflow":"CI","link":"https://github.com/ex/ex/actions/runs/1/job/42","description":""}]
BODY
    ;;
  pr-check-log)
    echo "gh api actions logs failed: HTTP 502" >&2
    exit 1
    ;;
  *)
    echo "stub: unknown subcommand $1" >&2
    exit 2
    ;;
esac
"#
    .to_string()
}

#[cfg(unix)]
#[test]
fn pr_checks_log_failed_marks_partial_log_fetch_failure() {
    let data_dir = tempfile::tempdir().unwrap();
    let plugin_root = tempfile::tempdir().unwrap();
    let body = pr_read_stub_plugin_failing_log();
    setup_pr_read_stub(data_dir.path(), plugin_root.path(), &body);

    // Outer check failure still drives the exit code even when the log
    // fetch itself fails for the failing job.
    let (stdout, _stderr) = run_fail(pr_read_cmd(data_dir.path(), plugin_root.path()).args([
        "pr",
        "checks",
        "--repo",
        "stub",
        "--number",
        "1",
        "--log-failed",
    ]));
    assert!(
        stdout.contains("===== Clippy (42) ====="),
        "header still emitted: {stdout}"
    );
    assert!(
        stdout.contains("(log unavailable:"),
        "Err branch marker: {stdout}"
    );
}

#[cfg(unix)]
#[test]
fn pr_checks_log_failed_suppressed_under_json() {
    let data_dir = tempfile::tempdir().unwrap();
    let plugin_root = tempfile::tempdir().unwrap();
    let body = pr_read_stub_plugin("{}", "[]", "[]", "error[E0308]: mismatched types");
    setup_pr_read_stub(data_dir.path(), plugin_root.path(), &body);

    let (stdout, _stderr) = run_fail(pr_read_cmd(data_dir.path(), plugin_root.path()).args([
        "pr",
        "checks",
        "--repo",
        "stub",
        "--number",
        "1",
        "--json",
        "--log-failed",
    ]));
    // JSON mode must emit a single valid JSON array with no log stream
    // leaking in and no "===== <job> =====" headers corrupting stdout.
    assert!(
        !stdout.contains("====="),
        "log headers leaked into JSON output: {stdout}"
    );
    let parsed: serde_json::Value =
        serde_json::from_str(&stdout).expect("stdout must be a single valid JSON array");
    assert!(parsed.is_array(), "expected JSON array, got {parsed}");
}

#[cfg(unix)]
#[test]
fn pr_view_surfaces_malformed_plugin_json_as_worksource_error() {
    // A plugin bug or a gh breaking-change that emits a shape-mismatched
    // blob must produce a loud WorkSource error, not an empty struct. The
    // rendered error does not need to be pretty, but the exit must be
    // non-zero and stderr must mention the PR so the failure site is
    // identifiable.
    let data_dir = tempfile::tempdir().unwrap();
    let plugin_root = tempfile::tempdir().unwrap();
    let body = pr_read_stub_plugin(
        "[this is not a valid ExternalPRDetails object]",
        "[]",
        "[]",
        "",
    );
    setup_pr_read_stub(data_dir.path(), plugin_root.path(), &body);

    let (_stdout, stderr) = run_fail(
        pr_read_cmd(data_dir.path(), plugin_root.path())
            .args(["pr", "view", "--repo", "stub", "--number", "1"]),
    );
    assert!(
        !stderr.is_empty(),
        "expected a non-empty error message on malformed plugin output"
    );
}

// ---------------------------------------------------------------------------
// Quality gate tests
// ---------------------------------------------------------------------------

/// `legion quality-gate record` writes a row and prints a UUIDv7 on stdout.
#[test]
fn quality_gate_record_prints_id() {
    let dir = tempfile::tempdir().unwrap();

    let id = run_ok(legion_cmd(dir.path()).args([
        "quality-gate",
        "record",
        "--skill",
        "legion-simplify",
        "--result",
        "clean",
    ]))
    .trim()
    .to_string();
    assert_uuid_format(&id);
}

/// `legion quality-gate record` with findings-count and details-json succeeds.
#[test]
fn quality_gate_record_with_details() {
    let dir = tempfile::tempdir().unwrap();
    let details = r#"{"result":"issues","findings_count":2,"findings":[]}"#;

    let id = run_ok(legion_cmd(dir.path()).args([
        "quality-gate",
        "record",
        "--skill",
        "legion-simplify",
        "--result",
        "issues",
        "--findings-count",
        "2",
        "--details-json",
        details,
    ]))
    .trim()
    .to_string();
    assert_uuid_format(&id);
}

/// `legion pr create` exits 1 with a clear error when no quality gate exists for HEAD.
///
/// The test uses a repo name that has no watch.toml entry, which means the gate
/// check fires first and the process exits before reaching worksource resolution.
/// This test is not #[ignore] because the gate check requires no work source.
#[test]
fn pr_create_refuses_without_quality_gate() {
    let dir = tempfile::tempdir().unwrap();

    // No gate recorded -- the DB is empty.

    let (_stdout, stderr) = run_fail(legion_cmd(dir.path()).args([
        "pr",
        "create",
        "--repo",
        "test-repo",
        "--title",
        "My PR",
    ]));
    assert!(
        stderr.contains("no clean legion-simplify gate") || stderr.contains("legion-simplify"),
        "error should mention legion-simplify gate, got: {stderr}"
    );
    assert!(
        stderr.contains("/legion-simplify") || stderr.contains("legion-simplify"),
        "error should guide toward the skill, got: {stderr}"
    );
}

/// `legion pr create --skip-gates` bypasses the quality gate, writes an audit entry,
/// and fails at worksource resolution (not at gate check).
///
/// This confirms --skip-gates exits past the gate. The specific exit message
/// from worksource failure varies by platform, so we only check the gate was not
/// the failure reason.
#[test]
fn pr_create_skip_gates_bypasses_gate_check() {
    let dir = tempfile::tempdir().unwrap();

    // No gate recorded, but --skip-gates should bypass that check.
    let out = legion_cmd(dir.path())
        .args([
            "pr",
            "create",
            "--repo",
            "no-such-repo",
            "--title",
            "Bootstrap PR",
            "--skip-gates",
        ])
        .output()
        .unwrap();

    // Should fail, but NOT with the "no clean legion-simplify gate" message.
    // It fails later because no watch.toml entry exists for "no-such-repo".
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr.contains("no clean legion-simplify gate"),
        "skip-gates should bypass gate error, got: {stderr}"
    );
}

/// Test that `legion sync` returns an error when no work source is configured.
/// This verifies the command parses and executes, even when it fails.
#[test]
fn sync_command_errors_without_worksource_config() {
    let data_dir = tempfile::tempdir().unwrap();

    // Run sync for a repo with no watch.toml entry - should fail gracefully

    let (_stdout, stderr) =
        run_fail(legion_cmd(data_dir.path()).args(["sync", "--repo", "nonexistent-repo"]));
    assert!(
        stderr.contains("no work source configured"),
        "expected 'no work source configured' error, got: {stderr}"
    );
}

/// Stub plugin that answers only `view-issue`, returning a fixed issue
/// whose body declares two acceptance criteria. Used by the pr write-check
/// gate tests; the unknown-verb fallback fails loud like the PR-read stub.
#[cfg(unix)]
fn view_issue_stub_plugin() -> String {
    r##"#!/bin/bash
set -e
case "${1:-}" in
  view-issue)
    cat <<'BODY'
{"url":"https://example.com/issues/7","number":7,"title":"stub issue","body":"Why it matters.\n\n## Acceptance criteria\n- crit one\n- crit two\n","labels":[],"assignees":null,"state":"OPEN"}
BODY
    ;;
  *)
    echo "stub: unknown subcommand $1" >&2
    exit 2
    ;;
esac
"##
    .to_string()
}

/// `legion pr write-check` with a substantive body: exits 0, reports the
/// gate clean, and counts one mapping entry per acceptance criterion
/// (#519 forcing function, #608 coverage net).
#[cfg(unix)]
#[test]
fn pr_write_check_passes_substantive_body_and_reports_clean() {
    let data_dir = tempfile::tempdir().unwrap();
    let plugin_root = tempfile::tempdir().unwrap();
    setup_pr_read_stub(
        data_dir.path(),
        plugin_root.path(),
        &view_issue_stub_plugin(),
    );

    let body = "## Summary\n\nDoes the thing.\n\n\
        ## Acceptance criteria mapping\n\n\
        ### 1. crit one\n\
        The handler now threads the flag through the dispatch table so the \
        first criterion is satisfied end to end.\n\
        Evidence: tests/integration/worksource_pr.rs::stub_test\n\n\
        ### 2. crit two\n\
        The second path is covered by the new guard clause, which refuses \
        the malformed input before it reaches the store.\n\
        Evidence: src/pr_write.rs:49 validate_pr_body\n\n\
        ## Not done\n\n\
        Did not migrate old rows -- out of scope, tracked separately.\n";
    let body_file = data_dir.path().join("pr-body.md");
    std::fs::write(&body_file, body).unwrap();

    let stdout = run_ok(pr_read_cmd(data_dir.path(), plugin_root.path()).args([
        "pr",
        "write-check",
        "--repo",
        "stub",
        "--issue",
        "7",
        "--body-file",
        body_file.to_str().unwrap(),
    ]));
    assert!(
        stdout.contains("pr-write gate clean"),
        "expected clean gate message, got: {stdout}"
    );
    assert!(
        stdout.contains("2 mapping entries"),
        "expected one mapping entry per criterion, got: {stdout}"
    );
}

/// `legion pr write-check` with a boilerplate body (no mapping section, no
/// not-done section): exits non-zero and lists the structural gaps.
#[cfg(unix)]
#[test]
fn pr_write_check_refuses_boilerplate_body_with_gaps() {
    let data_dir = tempfile::tempdir().unwrap();
    let plugin_root = tempfile::tempdir().unwrap();
    setup_pr_read_stub(
        data_dir.path(),
        plugin_root.path(),
        &view_issue_stub_plugin(),
    );

    let body_file = data_dir.path().join("pr-body.md");
    std::fs::write(&body_file, "## Summary\n\nDid stuff.\n").unwrap();

    let (_stdout, stderr) = run_fail(pr_read_cmd(data_dir.path(), plugin_root.path()).args([
        "pr",
        "write-check",
        "--repo",
        "stub",
        "--issue",
        "7",
        "--body-file",
        body_file.to_str().unwrap(),
    ]));
    assert!(
        stderr.contains("pr-write gate FAILED"),
        "expected gate failure banner, got: {stderr}"
    );
    assert!(
        stderr.contains("Acceptance criteria mapping"),
        "expected the missing-mapping finding, got: {stderr}"
    );
    assert!(
        stderr.contains("Not done"),
        "expected the missing not-done finding, got: {stderr}"
    );
}
