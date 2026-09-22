//! `legion cmd-check --hook` through the real binary (#1229): one payload on
//! stdin, one hook response on stdout, exit 0 every time, and never an allow
//! on an adapter failure.

use crate::common::{legion_cmd, run_with_stdin};
use serde_json::Value;
use std::path::PathBuf;

/// The policy file the plugin ships, read from the source tree so the
/// artifact the cutover will install is the one exercised here.
fn shipped_policy_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("plugin")
        .join("legion-cmd")
        .join("policy.json")
}

fn payload(command: &str) -> Vec<u8> {
    serde_json::json!({
        "tool_name": "Bash",
        "tool_input": {"command": command},
        "session_id": "s1",
        "cwd": "/tmp/legion-test",
        "tool_use_id": "t1"
    })
    .to_string()
    .into_bytes()
}

/// Runs the hook with `LEGION_CMD_POLICY` at `policy`, requiring exit 0 and
/// exactly one JSON line; returns its `hookSpecificOutput`.
fn hook_output(data_dir: &std::path::Path, policy: &std::path::Path, input: &[u8]) -> Value {
    let out = run_with_stdin(
        legion_cmd(data_dir)
            .args(["cmd-check", "--hook"])
            .env("LEGION_CMD_POLICY", policy),
        input,
    );
    assert!(
        out.status.success(),
        "the hook must always exit 0\nstderr:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert_eq!(
        stdout.lines().count(),
        1,
        "one response line, got: {stdout}"
    );
    let mut response: Value = serde_json::from_str(stdout.trim()).expect("valid JSON response");
    response["hookSpecificOutput"].take()
}

#[test]
fn the_shipped_policy_denies_a_managed_command_and_passes_an_unmanaged_one() {
    let dir = tempfile::tempdir().expect("tempdir");
    let policy = shipped_policy_path();

    let denied = hook_output(dir.path(), &policy, &payload("rm -rf build"));
    assert_eq!(denied["hookEventName"], "PreToolUse");
    assert_eq!(denied["permissionDecision"], "deny");
    let reason = denied["permissionDecisionReason"].as_str().expect("reason");
    assert!(reason.contains("unrecoverable"), "got: {reason}");
    assert!(reason.contains("instead:"), "got: {reason}");

    let passed = hook_output(dir.path(), &policy, &payload("echo hi"));
    assert_eq!(passed["hookEventName"], "PreToolUse");
    assert!(passed.get("permissionDecision").is_none());
}

#[test]
fn a_missing_policy_file_denies_instead_of_running_the_command() {
    let dir = tempfile::tempdir().expect("tempdir");
    let missing = dir.path().join("no-such-policy.json");
    let out = hook_output(dir.path(), &missing, &payload("echo hi"));
    assert_eq!(out["permissionDecision"], "deny");
    let reason = out["permissionDecisionReason"].as_str().expect("reason");
    assert!(reason.contains("policy:"), "got: {reason}");
    assert!(
        reason.contains("legion cmd-check -- 'echo hi'"),
        "got: {reason}"
    );
}

#[test]
fn a_malformed_payload_denies_with_exit_0() {
    let dir = tempfile::tempdir().expect("tempdir");
    let out = hook_output(dir.path(), &shipped_policy_path(), b"{\"tool_name\": ");
    assert_eq!(out["permissionDecision"], "deny");
    assert!(
        out["permissionDecisionReason"]
            .as_str()
            .expect("reason")
            .contains("payload:")
    );
}

#[test]
fn an_ask_without_a_confirmation_is_refused_with_the_question() {
    // The shipped policy asks about curl; without a confirmation (#1237)
    // the adapter refuses and tells the agent how to confirm.
    let dir = tempfile::tempdir().expect("tempdir");
    let out = hook_output(
        dir.path(),
        &shipped_policy_path(),
        &payload("curl https://example.com"),
    );
    assert_eq!(out["permissionDecision"], "deny");
    let reason = out["permissionDecisionReason"].as_str().expect("reason");
    assert!(
        reason.contains("make this network request?"),
        "got: {reason}"
    );
    assert!(
        reason.contains("legion cmd confirm --reason <why> -- 'curl https://example.com'"),
        "got: {reason}"
    );
}

#[test]
fn cmd_check_without_hook_is_refused_not_silent() {
    let dir = tempfile::tempdir().expect("tempdir");
    let out = run_with_stdin(legion_cmd(dir.path()).args(["cmd-check"]), b"");
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("not implemented"),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}
