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
    # Minimal fixture matching PrChecksResult shape (#736: headSha + checks)
    # so `pr checks --log-failed` has something to iterate. One failing
    # check, one success, plus one failure whose link does not match the
    # Actions pattern so the non-Actions-link branch runs.
    cat <<'BODY'
{{
  "headSha": "deadbeef",
  "checks": [
    {{"name":"Clippy","state":"FAILURE","workflow":"CI","link":"https://github.com/ex/ex/actions/runs/1/job/42","description":""}},
    {{"name":"Tests","state":"SUCCESS","workflow":"CI","link":"https://github.com/ex/ex/actions/runs/1/job/99","description":""}},
    {{"name":"External","state":"FAILURE","workflow":"CI","link":"https://dashboard.example.com/checks/abc","description":""}}
  ]
}}
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
{"headSha":"deadbeef","checks":[{"name":"Clippy","state":"FAILURE","workflow":"CI","link":"https://github.com/ex/ex/actions/runs/1/job/42","description":""}]}
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

// ---------------------------------------------------------------------------
// #736: pr-checks pinned to the PR head SHA. A plugin response distinguishes
// "checks reported for this exact head commit" from "zero runs for the head"
// -- the latter must be a distinct non-passing state, not a silent pass
// inherited from wherever the branch's last suite happened to be.
// ---------------------------------------------------------------------------

#[cfg(unix)]
fn pr_checks_only_stub(pr_checks_body: &str) -> String {
    format!(
        r#"#!/bin/bash
set -e
case "${{1:-}}" in
  pr-checks)
    cat <<'BODY'
{pr_checks_body}
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

/// A plugin that resolved zero check-runs for the PR's head SHA must fail
/// `legion pr checks` with a distinct, named state -- not silently exit 0
/// because an empty `checks` array has nothing to iterate and "finds
/// nothing failing" (the exact false-green shape observed live on PR #735).
#[cfg(unix)]
#[test]
fn pr_checks_zero_runs_for_head_is_non_passing() {
    let data_dir = tempfile::tempdir().unwrap();
    let plugin_root = tempfile::tempdir().unwrap();
    let body = pr_checks_only_stub(r#"{"headSha":"ec3825b","checks":[]}"#);
    setup_pr_read_stub(data_dir.path(), plugin_root.path(), &body);

    let (_stdout, stderr) = run_fail(
        pr_read_cmd(data_dir.path(), plugin_root.path())
            .args(["pr", "checks", "--repo", "stub", "--number", "735"]),
    );
    assert!(
        stderr.contains("no runs for head ec3825b"),
        "expected 'no runs for head <sha>' in stderr, got: {stderr}"
    );
}

/// Contrast case: when the plugin reports checks FOR THE HEAD SHA and they
/// are all in a passing state, `legion pr checks` must exit clean. Guards
/// against a fix that over-corrects into always failing.
#[cfg(unix)]
#[test]
fn pr_checks_head_sha_with_passing_runs_succeeds() {
    let data_dir = tempfile::tempdir().unwrap();
    let plugin_root = tempfile::tempdir().unwrap();
    let body = pr_checks_only_stub(
        r#"{"headSha":"abc1234","checks":[{"name":"Tests","state":"SUCCESS","workflow":"CI","link":"https://github.com/ex/ex/actions/runs/1/job/1","description":""}]}"#,
    );
    setup_pr_read_stub(data_dir.path(), plugin_root.path(), &body);

    let stdout = run_ok(
        pr_read_cmd(data_dir.path(), plugin_root.path())
            .args(["pr", "checks", "--repo", "stub", "--number", "1"]),
    );
    assert!(stdout.contains("SUCCESS"));
    assert!(stdout.contains("Tests"));
}

/// `legion pr merge` must refuse with the SAME "no runs for head <sha>"
/// wording as `legion pr checks` (#736 criterion 3) -- and must refuse
/// before ever invoking the plugin's `merge` subcommand, which this stub
/// does not implement (a call to it would fail the test with "unknown
/// subcommand" instead of the expected refusal, proving merge is gated).
#[cfg(unix)]
#[test]
fn pr_merge_refuses_when_head_has_zero_runs() {
    let data_dir = tempfile::tempdir().unwrap();
    let plugin_root = tempfile::tempdir().unwrap();
    let body = pr_checks_only_stub(r#"{"headSha":"ec3825b","checks":[]}"#);
    setup_pr_read_stub(data_dir.path(), plugin_root.path(), &body);

    let (_stdout, stderr) = run_fail(
        pr_read_cmd(data_dir.path(), plugin_root.path())
            .args(["pr", "merge", "--repo", "stub", "--number", "735"]),
    );
    assert!(
        stderr.contains("no runs for head ec3825b"),
        "expected the same 'no runs for head <sha>' refusal as `pr checks`, got: {stderr}"
    );
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
    // #720: the error must name the operation (view-pr), not just the
    // offending field, so the operator can tell which of a dozen worksource
    // calls actually failed.
    assert!(
        stderr.contains("view-pr"),
        "expected the operation name in the error, got: {stderr}"
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

// --- #610: require_worksource + Done close-propagation fold ---

/// Stub worksource plugin whose `close` verb exits with the given code.
/// Used by the require_worksource configured-path test and the Done
/// propagation tests; the unknown-verb fallback fails loud like the
/// PR-read stub.
#[cfg(unix)]
fn close_stub_plugin(close_exit: i32) -> String {
    format!(
        r##"#!/bin/bash
case "${{1:-}}" in
  close)
    exit {close_exit}
    ;;
  *)
    echo "stub: unknown subcommand $1" >&2
    exit 2
    ;;
esac
"##
    )
}

/// #610: `require_worksource` is now the single source of the fatal
/// "no work source configured" error text. Assert the full canonical
/// message (not just the prefix) so drift in the pinned literal is caught.
#[test]
fn require_worksource_missing_emits_canonical_error() {
    let dir = tempfile::tempdir().unwrap();

    let (_stdout, stderr) = run_fail(legion_cmd(dir.path()).args([
        "issue",
        "close",
        "--repo",
        "no-such-repo",
        "--number",
        "7",
    ]));
    assert!(
        stderr.contains("no work source configured for repo 'no-such-repo' in watch.toml"),
        "expected the canonical require_worksource error, got: {stderr}"
    );
}

/// #610: with a configured work source, `require_worksource` resolves the
/// (plugin, source_repo, workdir) tuple and the command proceeds to the
/// plugin. Also covers the threaded db handle: `issue close` now opens one
/// connection up front and the audit row must land.
#[cfg(unix)]
#[test]
fn issue_close_with_configured_worksource_writes_audit_row() {
    let data_dir = tempfile::tempdir().unwrap();
    let plugin_root = tempfile::tempdir().unwrap();
    setup_pr_read_stub(data_dir.path(), plugin_root.path(), &close_stub_plugin(0));

    let stdout = run_ok(
        pr_read_cmd(data_dir.path(), plugin_root.path())
            .args(["issue", "close", "--repo", "stub", "--number", "42"]),
    );
    assert!(
        stdout.contains("closed issue #42 on owner/stub"),
        "expected close confirmation, got: {stdout}"
    );

    let audit_out = run_ok(legion_cmd(data_dir.path()).args(["audit", "--action", "close-issue"]));
    assert!(
        audit_out.contains("close-issue") && audit_out.contains("#42"),
        "expected a close-issue audit row for #42, got: {audit_out}"
    );
}

/// Create a card on repo `stub` linked to external issue #42 and promote
/// it to a Done-eligible state (Backlog -> assign -> accept). Shared by
/// the Done propagation tests.
#[cfg(unix)]
fn setup_linked_done_eligible_card(data_dir: &std::path::Path) -> String {
    let card = run_ok(legion_cmd(data_dir).args([
        "kanban",
        "create",
        "--from",
        "sean",
        "--to",
        "stub",
        "--text",
        "linked work",
        "--source-url",
        "https://github.com/owner/stub/issues/42",
        "--source-type",
        "github",
    ]))
    .trim()
    .to_string();
    run_ok(legion_cmd(data_dir).args(["kanban", "assign", "--id", &card, "--to", "stub"]));
    run_ok(legion_cmd(data_dir).args(["kanban", "accept", "--id", &card]));
    card
}

/// #610 behavior fix: `legion done --id` with a linked external issue now
/// folds through `propagate_card_close_to_worksource`, so the close writes
/// the same audit row `legion kanban cancel` writes. The inline copy this
/// replaced closed the issue with no audit trail.
#[cfg(unix)]
#[test]
fn done_with_linked_issue_propagates_close_and_writes_audit_row() {
    let data_dir = tempfile::tempdir().unwrap();
    let plugin_root = tempfile::tempdir().unwrap();
    setup_pr_read_stub(data_dir.path(), plugin_root.path(), &close_stub_plugin(0));
    let card = setup_linked_done_eligible_card(data_dir.path());

    let stderr = run_ok_stderr(
        pr_read_cmd(data_dir.path(), plugin_root.path())
            .args(["done", "--repo", "stub", "--text", "shipped", "--id", &card]),
    );
    assert!(
        stderr.contains("closed github issue #42"),
        "expected the propagation breadcrumb, got: {stderr}"
    );

    let audit_out = run_ok(legion_cmd(data_dir.path()).args(["audit", "--action", "close-issue"]));
    assert!(
        audit_out.contains("#42"),
        "expected a close-issue audit row for the propagated close, got: {audit_out}"
    );
    assert!(
        audit_out.contains(&card),
        "expected the audit row to carry the card id as task, got: {audit_out}"
    );
}

/// #610: when close propagation fails, `legion done` still succeeds (the
/// card is Done locally) but emits the same stdout WARNING line
/// `legion kanban cancel` emits, instead of the old stderr-only failure
/// scripted callers could not see.
#[cfg(unix)]
#[test]
fn done_propagation_failure_warns_on_stdout() {
    let data_dir = tempfile::tempdir().unwrap();
    let plugin_root = tempfile::tempdir().unwrap();
    setup_pr_read_stub(data_dir.path(), plugin_root.path(), &close_stub_plugin(1));
    let card = setup_linked_done_eligible_card(data_dir.path());

    let stdout = run_ok(
        pr_read_cmd(data_dir.path(), plugin_root.path())
            .args(["done", "--repo", "stub", "--text", "shipped", "--id", &card]),
    );
    assert!(
        stdout.contains("propagation FAILED"),
        "expected the stdout partial-failure warning, got: {stdout}"
    );
}

// --- quality-gate list + stats integration tests (#666) ---

/// `legion quality-gate list` with no rows prints nothing and exits 0.
#[test]
fn quality_gate_list_empty_exits_zero_with_no_output() {
    let dir = tempfile::tempdir().unwrap();
    let stdout = run_ok(legion_cmd(dir.path()).args(["quality-gate", "list"]));
    assert!(
        stdout.is_empty(),
        "expected no output for empty gate corpus, got: {stdout}"
    );
}

/// `legion quality-gate list --json` with no rows prints `[]` and exits 0.
#[test]
fn quality_gate_list_empty_json_prints_empty_array() {
    let dir = tempfile::tempdir().unwrap();
    let stdout = run_ok(legion_cmd(dir.path()).args(["quality-gate", "list", "--json"]));
    assert_eq!(
        stdout.trim(),
        "[]",
        "expected [] for empty gate corpus with --json, got: {stdout}"
    );
}

/// `legion quality-gate list` shows recorded rows in the human table.
#[test]
fn quality_gate_list_shows_recorded_rows() {
    let dir = tempfile::tempdir().unwrap();

    // Seed two rows with different skills.
    run_ok(legion_cmd(dir.path()).args([
        "quality-gate",
        "record",
        "--skill",
        "legion-simplify",
        "--result",
        "clean",
    ]));
    run_ok(legion_cmd(dir.path()).args([
        "quality-gate",
        "record",
        "--skill",
        "legion-review",
        "--result",
        "issues",
        "--findings-count",
        "3",
    ]));

    let stdout = run_ok(legion_cmd(dir.path()).args(["quality-gate", "list"]));
    assert!(
        stdout.contains("legion-simplify"),
        "expected simplify row in list output, got: {stdout}"
    );
    assert!(
        stdout.contains("legion-review"),
        "expected review row in list output, got: {stdout}"
    );
    assert!(
        stdout.contains("clean"),
        "expected 'clean' result in list output, got: {stdout}"
    );
    assert!(
        stdout.contains("issues"),
        "expected 'issues' result in list output, got: {stdout}"
    );
}

/// `legion quality-gate list --skill` filters to the named skill only.
#[test]
fn quality_gate_list_filter_by_skill() {
    let dir = tempfile::tempdir().unwrap();

    run_ok(legion_cmd(dir.path()).args([
        "quality-gate",
        "record",
        "--skill",
        "legion-simplify",
        "--result",
        "clean",
    ]));
    run_ok(legion_cmd(dir.path()).args([
        "quality-gate",
        "record",
        "--skill",
        "legion-review",
        "--result",
        "issues",
    ]));

    let stdout =
        run_ok(legion_cmd(dir.path()).args(["quality-gate", "list", "--skill", "legion-review"]));
    assert!(
        stdout.contains("legion-review"),
        "expected review row, got: {stdout}"
    );
    assert!(
        !stdout.contains("legion-simplify"),
        "simplify row should be filtered out, got: {stdout}"
    );
}

/// `legion quality-gate list --result issues` filters to issues-only rows.
#[test]
fn quality_gate_list_filter_by_result() {
    let dir = tempfile::tempdir().unwrap();

    run_ok(legion_cmd(dir.path()).args([
        "quality-gate",
        "record",
        "--skill",
        "s",
        "--result",
        "clean",
    ]));
    run_ok(legion_cmd(dir.path()).args([
        "quality-gate",
        "record",
        "--skill",
        "s",
        "--result",
        "issues",
        "--findings-count",
        "2",
    ]));

    let stdout =
        run_ok(legion_cmd(dir.path()).args(["quality-gate", "list", "--result", "issues"]));
    // Exactly one row (issues); the clean row must be filtered out. Assert both
    // that the issues row is present (findings_count=2) AND that the clean row is
    // absent -- "clean" appears nowhere in the header or the issues row, so its
    // absence confirms the filter where contains("2") alone could not.
    assert!(
        stdout.contains("2"),
        "expected findings_count=2 in the issues row, got: {stdout}"
    );
    assert!(
        !stdout.contains("clean"),
        "expected the clean row to be filtered out, got: {stdout}"
    );
}

/// `legion quality-gate list --result bad-value` exits non-zero with a typed error.
#[test]
fn quality_gate_list_invalid_result_value_exits_nonzero() {
    let dir = tempfile::tempdir().unwrap();
    // clap value_parser rejects unknown values before the handler runs.
    let (_stdout, stderr) =
        run_fail(legion_cmd(dir.path()).args(["quality-gate", "list", "--result", "bad-value"]));
    // clap emits an error mentioning the bad value or the possible values.
    assert!(
        stderr.contains("bad-value") || stderr.contains("possible values"),
        "expected error about invalid result value, got: {stderr}"
    );
}

/// `legion quality-gate list --json` emits a JSON array with all fields including details.
#[test]
fn quality_gate_list_json_emits_array_with_details() {
    let dir = tempfile::tempdir().unwrap();
    let details = r#"{"findings":[]}"#;

    run_ok(legion_cmd(dir.path()).args([
        "quality-gate",
        "record",
        "--skill",
        "legion-simplify",
        "--result",
        "issues",
        "--findings-count",
        "1",
        "--details-json",
        details,
    ]));

    let stdout = run_ok(legion_cmd(dir.path()).args(["quality-gate", "list", "--json"]));
    let parsed: serde_json::Value =
        serde_json::from_str(&stdout).expect("expected valid JSON array");
    let arr = parsed.as_array().expect("expected a JSON array");
    assert_eq!(arr.len(), 1, "expected one row");

    let row = &arr[0];
    assert_eq!(row["skill"].as_str().unwrap(), "legion-simplify");
    assert_eq!(row["result"].as_str().unwrap(), "issues");
    assert_eq!(row["findings_count"].as_u64().unwrap(), 1);
    // details field must be present in JSON output.
    assert!(
        row["details"].is_string() || row["details"].is_null(),
        "expected details field in JSON row"
    );
}

/// `legion quality-gate stats` with no rows prints nothing and exits 0.
#[test]
fn quality_gate_stats_empty_exits_zero_with_no_output() {
    let dir = tempfile::tempdir().unwrap();
    let stdout = run_ok(legion_cmd(dir.path()).args(["quality-gate", "stats"]));
    assert!(
        stdout.is_empty(),
        "expected no output for empty gate corpus, got: {stdout}"
    );
}

/// `legion quality-gate stats --json` with no rows prints `[]`.
#[test]
fn quality_gate_stats_empty_json_prints_empty_array() {
    let dir = tempfile::tempdir().unwrap();
    let stdout = run_ok(legion_cmd(dir.path()).args(["quality-gate", "stats", "--json"]));
    assert_eq!(
        stdout.trim(),
        "[]",
        "expected [] for empty stats with --json, got: {stdout}"
    );
}

/// `legion quality-gate stats` shows per-skill aggregates in the human table.
#[test]
fn quality_gate_stats_shows_per_skill_aggregates() {
    let dir = tempfile::tempdir().unwrap();

    // 2 runs for legion-simplify: 1 clean, 1 issues (1 finding).
    run_ok(legion_cmd(dir.path()).args([
        "quality-gate",
        "record",
        "--skill",
        "legion-simplify",
        "--result",
        "clean",
    ]));
    run_ok(legion_cmd(dir.path()).args([
        "quality-gate",
        "record",
        "--skill",
        "legion-simplify",
        "--result",
        "issues",
        "--findings-count",
        "1",
    ]));

    let stdout = run_ok(legion_cmd(dir.path()).args(["quality-gate", "stats"]));
    assert!(
        stdout.contains("legion-simplify"),
        "expected skill row in stats output, got: {stdout}"
    );
    // 2 runs total.
    assert!(
        stdout.contains('2'),
        "expected run count of 2, got: {stdout}"
    );
}

/// `legion quality-gate stats --json` returns a JSON array with catch_rate field.
#[test]
fn quality_gate_stats_json_shape() {
    let dir = tempfile::tempdir().unwrap();

    // 3 runs: 1 clean, 2 issues.
    for _ in 0..2 {
        run_ok(legion_cmd(dir.path()).args([
            "quality-gate",
            "record",
            "--skill",
            "s",
            "--result",
            "issues",
            "--findings-count",
            "1",
        ]));
    }
    run_ok(legion_cmd(dir.path()).args([
        "quality-gate",
        "record",
        "--skill",
        "s",
        "--result",
        "clean",
    ]));

    let stdout = run_ok(legion_cmd(dir.path()).args(["quality-gate", "stats", "--json"]));
    let parsed: serde_json::Value =
        serde_json::from_str(&stdout).expect("expected valid JSON array");
    let arr = parsed.as_array().expect("expected a JSON array");
    assert_eq!(arr.len(), 1);

    let row = &arr[0];
    assert_eq!(row["skill"].as_str().unwrap(), "s");
    assert_eq!(row["runs"].as_u64().unwrap(), 3);
    assert_eq!(row["clean"].as_u64().unwrap(), 1);
    assert_eq!(row["issues"].as_u64().unwrap(), 2);
    // catch_rate = 2/3 ~= 0.6667
    let catch_rate = row["catch_rate"].as_f64().unwrap();
    assert!(
        (catch_rate - 2.0 / 3.0).abs() < 1e-6,
        "catch_rate should be ~0.6667, got: {catch_rate}"
    );
    assert_eq!(row["total_findings"].as_u64().unwrap(), 2);
    assert_eq!(row["max_findings"].as_u64().unwrap(), 1);
}

/// `legion quality-gate stats --skill` filters the aggregate to that skill only.
#[test]
fn quality_gate_stats_filter_by_skill() {
    let dir = tempfile::tempdir().unwrap();

    run_ok(legion_cmd(dir.path()).args([
        "quality-gate",
        "record",
        "--skill",
        "legion-simplify",
        "--result",
        "clean",
    ]));
    run_ok(legion_cmd(dir.path()).args([
        "quality-gate",
        "record",
        "--skill",
        "legion-review",
        "--result",
        "issues",
    ]));

    let stdout = run_ok(legion_cmd(dir.path()).args([
        "quality-gate",
        "stats",
        "--skill",
        "legion-simplify",
        "--json",
    ]));
    let arr: Vec<serde_json::Value> = serde_json::from_str(&stdout).expect("expected JSON array");
    assert_eq!(arr.len(), 1, "expected only one skill row");
    assert_eq!(arr[0]["skill"].as_str().unwrap(), "legion-simplify");
}

// ---------------------------------------------------------------------------
// quality-gate check end-to-end tests (#665).
//
// These tests require a real git repo so `git_changed_files` can probe base
// refs and run `git diff`. Each test creates a temp repo with a `main` branch
// and a feature branch, writes an articulation file to a second tempdir, and
// runs `legion quality-gate check` via `legion_cmd` against a separate data dir.
// ---------------------------------------------------------------------------

/// Set up a minimal git repo with a `main` branch (one seed commit) and a
/// feature branch with one changed file (`src/foo.rs`). Returns the repo dir.
///
/// Every git invocation runs via `run_git_fixture`: explicit tempdir
/// `current_dir` plus per-invocation `-c user.name`/`-c user.email`/`-c
/// commit.gpgsign=false` -- never a `git config` write -- so the fixture
/// cannot poison the enclosing real checkout's config (#723).
fn setup_git_repo_with_feature_branch() -> tempfile::TempDir {
    let repo = tempfile::tempdir().unwrap();
    let rp = repo.path();

    run_git_fixture(rp, &["init", "-b", "main"]);

    // Seed commit on main so `main` resolves as a real ref.
    std::fs::write(rp.join("README.md"), "seed\n").unwrap();
    run_git_fixture(rp, &["add", "README.md"]);
    run_git_fixture(rp, &["commit", "-m", "seed"]);

    // Feature branch: add one changed file.
    run_git_fixture(rp, &["checkout", "-b", "feat/test"]);
    std::fs::create_dir_all(rp.join("src")).unwrap();
    std::fs::write(rp.join("src/foo.rs"), "// changed\n").unwrap();
    run_git_fixture(rp, &["add", "src/foo.rs"]);
    run_git_fixture(rp, &["commit", "-m", "add foo"]);

    repo
}

/// Build a substantive articulation that covers `src/foo.rs` with enough prose
/// to clear the word-count threshold.
fn good_articulation_for_foo() -> String {
    "# Simplify articulation\n\n\
     ### src/foo.rs\n\
     Checked for duplicate logic, unnecessary abstraction, stringly-typed state, \
     hand-rolled standard library dupes, copy-paste variation, and error swallowing. \
     No issues found: `fn foo` at src/foo.rs:1 has a single responsibility and the \
     error handling propagates via the `?` operator throughout.\n"
        .to_string()
}

/// quality-gate check: a valid articulation passes, records exactly one gate
/// row for HEAD, and the row is visible via `quality-gate list --json`.
#[cfg(unix)]
#[test]
fn quality_gate_check_passing_articulation_records_gate_row() {
    let _config_guard = RealRepoConfigGuard::new();
    let repo = setup_git_repo_with_feature_branch();
    let data_dir = tempfile::tempdir().unwrap();
    let artic_file = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(artic_file.path(), good_articulation_for_foo()).unwrap();

    let stdout = run_ok(legion_cmd(data_dir.path()).current_dir(repo.path()).args([
        "quality-gate",
        "check",
        "--skill",
        "legion-simplify",
        "--result",
        "clean",
        "--articulation-file",
        artic_file.path().to_str().unwrap(),
    ]));
    assert!(
        stdout.contains("accepted"),
        "expected acceptance message, got: {stdout}"
    );

    // Exactly one gate row must be recorded.
    let list_out = run_ok(legion_cmd(data_dir.path()).args([
        "quality-gate",
        "list",
        "--skill",
        "legion-simplify",
        "--json",
    ]));
    let rows: serde_json::Value = serde_json::from_str(&list_out).expect("expected JSON");
    let arr = rows.as_array().expect("expected array");
    assert_eq!(
        arr.len(),
        1,
        "expected exactly one gate row, got: {list_out}"
    );
    assert_eq!(arr[0]["result"].as_str().unwrap(), "clean");
}

/// quality-gate check: a failing articulation (missing coverage) exits non-zero
/// AND records NO gate row. This is the security-relevant path: the absence of
/// a row is what blocks `legion pr create` from proceeding.
#[cfg(unix)]
#[test]
fn quality_gate_check_failing_articulation_exits_nonzero_and_records_no_row() {
    let _config_guard = RealRepoConfigGuard::new();
    let repo = setup_git_repo_with_feature_branch();
    let data_dir = tempfile::tempdir().unwrap();
    // Articulation is empty -- no entries at all, so src/foo.rs is uncovered.
    let artic_file = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(artic_file.path(), "# No entries\n").unwrap();

    let (_stdout, stderr) = run_fail(legion_cmd(data_dir.path()).current_dir(repo.path()).args([
        "quality-gate",
        "check",
        "--skill",
        "legion-simplify",
        "--result",
        "clean",
        "--articulation-file",
        artic_file.path().to_str().unwrap(),
    ]));
    assert!(
        stderr.contains("FAILED") || stderr.contains("missing coverage"),
        "expected failure message in stderr, got: {stderr}"
    );

    // No row must have been recorded -- the gate on HEAD is absent.
    let list_out = run_ok(legion_cmd(data_dir.path()).args([
        "quality-gate",
        "list",
        "--skill",
        "legion-simplify",
        "--json",
    ]));
    let rows: serde_json::Value = serde_json::from_str(&list_out).expect("expected JSON");
    let arr = rows.as_array().expect("expected array");
    assert_eq!(
        arr.len(),
        0,
        "expected zero gate rows after articulation refusal, got: {list_out}"
    );
}

/// quality-gate check: when run outside a git repo, git_changed_files returns
/// a hard error and the command exits non-zero without recording a gate row.
/// This exercises the "git not in work tree" branch introduced in #665.
#[cfg(unix)]
#[test]
fn quality_gate_check_outside_git_repo_exits_nonzero() {
    // /tmp is never inside a git repo.
    let outside_git = std::path::PathBuf::from("/tmp");
    let data_dir = tempfile::tempdir().unwrap();
    let artic_file = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(artic_file.path(), good_articulation_for_foo()).unwrap();

    let (_stdout, stderr) = run_fail(legion_cmd(data_dir.path()).current_dir(&outside_git).args([
        "quality-gate",
        "check",
        "--skill",
        "legion-simplify",
        "--result",
        "clean",
        "--articulation-file",
        artic_file.path().to_str().unwrap(),
    ]));
    // The error must name git or the work-tree context.
    assert!(
        stderr.contains("git") || stderr.contains("work-tree") || stderr.contains("repo"),
        "expected a git/repo error for non-git directory, got: {stderr}"
    );

    // No gate row must have been recorded.
    let list_out = run_ok(legion_cmd(data_dir.path()).args([
        "quality-gate",
        "list",
        "--skill",
        "legion-simplify",
        "--json",
    ]));
    let rows: serde_json::Value = serde_json::from_str(&list_out).expect("expected JSON");
    assert_eq!(
        rows.as_array().unwrap().len(),
        0,
        "expected zero gate rows after git failure, got: {list_out}"
    );
}

/// quality-gate check: when run in a git repo that has no main/origin/main ref
/// AND HEAD has a parent commit, the command exits non-zero with a descriptive
/// error. This exercises the "base ref missing but HEAD has parent" hard-error
/// branch -- the path that previously passed vacuously.
#[cfg(unix)]
#[test]
fn quality_gate_check_no_base_ref_with_parent_commit_exits_nonzero() {
    let _config_guard = RealRepoConfigGuard::new();
    let repo = tempfile::tempdir().unwrap();
    let rp = repo.path();

    // Use a non-standard default branch name so neither `main` nor
    // `origin/main` is present. The repo has two commits (seed + feature),
    // so HEAD does have a parent.
    run_git_fixture(rp, &["init", "-b", "trunk"]);
    std::fs::write(rp.join("README.md"), "seed\n").unwrap();
    run_git_fixture(rp, &["add", "README.md"]);
    run_git_fixture(rp, &["commit", "-m", "seed"]);
    std::fs::write(rp.join("change.rs"), "// changed\n").unwrap();
    run_git_fixture(rp, &["add", "change.rs"]);
    run_git_fixture(rp, &["commit", "-m", "feature"]);

    let data_dir = tempfile::tempdir().unwrap();
    let artic_file = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(artic_file.path(), good_articulation_for_foo()).unwrap();

    let (_stdout, stderr) = run_fail(legion_cmd(data_dir.path()).current_dir(rp).args([
        "quality-gate",
        "check",
        "--skill",
        "legion-simplify",
        "--result",
        "clean",
        "--articulation-file",
        artic_file.path().to_str().unwrap(),
    ]));
    assert!(
        stderr.contains("main") || stderr.contains("base ref"),
        "expected error about missing base ref, got: {stderr}"
    );

    // No gate row recorded.
    let list_out = run_ok(legion_cmd(data_dir.path()).args([
        "quality-gate",
        "list",
        "--skill",
        "legion-simplify",
        "--json",
    ]));
    let rows: serde_json::Value = serde_json::from_str(&list_out).expect("expected JSON");
    assert_eq!(
        rows.as_array().unwrap().len(),
        0,
        "expected zero gate rows after base-ref-missing error, got: {list_out}"
    );
}

/// quality-gate check: non-ASCII file paths with core.quotePath are handled
/// correctly. The path returned by git (unescaped, with core.quotePath=false)
/// must match the `### <path>` heading, and the articulation must pass.
#[cfg(unix)]
#[test]
fn quality_gate_check_non_ascii_path_roundtrips_correctly() {
    let _config_guard = RealRepoConfigGuard::new();
    let repo = tempfile::tempdir().unwrap();
    let rp = repo.path();

    run_git_fixture(rp, &["init", "-b", "main"]);
    // Seed commit on main.
    std::fs::write(rp.join("README.md"), "seed\n").unwrap();
    run_git_fixture(rp, &["add", "README.md"]);
    run_git_fixture(rp, &["commit", "-m", "seed"]);

    // Feature branch: add a file with a non-ASCII name.
    run_git_fixture(rp, &["checkout", "-b", "feat/nonascii"]);
    std::fs::create_dir_all(rp.join("src")).unwrap();
    // Use a UTF-8 filename. The file system must support this; macOS and
    // Linux UTF-8 locales both do.
    let nonascii_name = "src/cafe\u{0301}.rs"; // NFC: src/café.rs (combining acute)
    std::fs::write(rp.join(nonascii_name), "// non-ascii\n").unwrap();
    run_git_fixture(rp, &["add", nonascii_name]);
    run_git_fixture(rp, &["commit", "-m", "add non-ascii file"]);

    // Ask git what name it reports (with core.quotePath=false).
    let diff_out = Command::new("git")
        .args([
            "-c",
            "core.quotePath=false",
            "diff",
            "--name-only",
            "main...HEAD",
        ])
        .current_dir(rp)
        .output()
        .expect("git diff failed");
    let reported_path = String::from_utf8_lossy(&diff_out.stdout).trim().to_string();
    // The path must be non-empty and not octal-quoted.
    assert!(
        !reported_path.is_empty(),
        "expected a path from git diff, got empty output"
    );
    assert!(
        !reported_path.starts_with('"'),
        "expected unquoted path, got: {reported_path}"
    );

    // Build an articulation that uses exactly the path git reported.
    let articulation = format!(
        "### {reported_path}\n\
         Checked for duplicate logic, unnecessary abstraction, stringly-typed state, \
         hand-rolled standard library dupes, copy-paste variation, and error swallowing. \
         No issues found: a single trivial file with no structure. Verdict: clean.\n\
         Evidence: {reported_path}:1 the lone declaration.\n"
    );

    let data_dir = tempfile::tempdir().unwrap();
    let artic_file = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(artic_file.path(), &articulation).unwrap();

    let stdout = run_ok(legion_cmd(data_dir.path()).current_dir(rp).args([
        "quality-gate",
        "check",
        "--skill",
        "legion-simplify",
        "--result",
        "clean",
        "--articulation-file",
        artic_file.path().to_str().unwrap(),
    ]));
    assert!(
        stdout.contains("accepted"),
        "expected acceptance for non-ASCII path, got: {stdout}"
    );
}
