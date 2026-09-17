//! Integration tests for `legion cmd-check --hook` (#1229): the real
//! subprocess, fed real hook JSON on stdin, reading a real policy file from
//! disk via `LEGION_CMD_POLICY`. Unlike `src/cmd/hook.rs`'s unit tests
//! (which call `build_response` in-process), these exercise the actual
//! binary boundary: process spawn, stdin/stdout plumbing, and exit code.

use crate::common::{legion_cmd, run_with_stdin};
use std::path::Path;
use tempfile::TempDir;

fn write_policy(dir: &Path, contents: &str) -> std::path::PathBuf {
    let path = dir.join("policy.json");
    std::fs::write(&path, contents).expect("write fixture policy");
    path
}

const FIXTURE_POLICY: &str = r#"{
  "tools": {
    "Bash": {
      "kind": "bash",
      "families": {
        "chmod": {
          "rules": [
            {
              "id": "chmod-777",
              "predicate": { "operand_contains": "777" },
              "outcome": { "kind": "deny", "reason": "opens the tree to everyone", "instead": "chmod 755" }
            }
          ]
        },
        "gh issue": {
          "rules": [
            {
              "id": "gh-issue-list",
              "predicate": { "arg_equals": "list" },
              "outcome": { "kind": "rewrite", "target": "legion issue list", "reason": "duplicate surface" }
            }
          ]
        },
        "gh pr": {
          "rules": [
            {
              "id": "gh-pr-merge-admin",
              "predicate": { "arg_equals": "--admin" },
              "outcome": { "kind": "ask", "question": "bypass branch protection?", "reason": "skips checks", "needs_operator": true }
            }
          ]
        }
      }
    }
  },
  "sym_jobs": []
}"#;

fn hook_payload(tool_name: &str, command: &str) -> Vec<u8> {
    serde_json::json!({
        "tool_name": tool_name,
        "tool_input": {"command": command},
        "session_id": "test-session",
        "cwd": "/repo/legion",
        "tool_use_id": "tool-use-1"
    })
    .to_string()
    .into_bytes()
}

fn hook_specific(stdout: &[u8]) -> serde_json::Value {
    let value: serde_json::Value = serde_json::from_slice(stdout).expect("stdout is valid JSON");
    value
        .get("hookSpecificOutput")
        .cloned()
        .expect("response has hookSpecificOutput")
}

#[test]
fn an_unmatched_command_allows_with_no_permission_decision() {
    let tmp = TempDir::new().expect("tempdir");
    let policy_path = write_policy(tmp.path(), FIXTURE_POLICY);

    let mut cmd = legion_cmd(tmp.path());
    cmd.arg("cmd-check")
        .arg("--hook")
        .env("LEGION_CMD_POLICY", &policy_path)
        .env_remove("CLAUDE_PLUGIN_ROOT");
    let out = run_with_stdin(&mut cmd, &hook_payload("Bash", "ls -la"));

    assert!(out.status.success(), "run_hook always exits 0");
    let out_value = hook_specific(&out.stdout);
    assert!(out_value.get("permissionDecision").is_none());
}

#[test]
fn a_governed_command_denies_naming_reason_and_instead() {
    let tmp = TempDir::new().expect("tempdir");
    let policy_path = write_policy(tmp.path(), FIXTURE_POLICY);

    let mut cmd = legion_cmd(tmp.path());
    cmd.arg("cmd-check")
        .arg("--hook")
        .env("LEGION_CMD_POLICY", &policy_path)
        .env_remove("CLAUDE_PLUGIN_ROOT");
    let out = run_with_stdin(&mut cmd, &hook_payload("Bash", "chmod 777 x"));

    assert!(out.status.success());
    let out_value = hook_specific(&out.stdout);
    assert_eq!(out_value["permissionDecision"], "deny");
    let reason = out_value["permissionDecisionReason"].as_str().unwrap();
    assert!(reason.contains("opens the tree to everyone"));
    assert!(reason.contains("chmod 755"));
}

#[test]
fn a_rewrite_returns_updated_input_and_allow() {
    let tmp = TempDir::new().expect("tempdir");
    let policy_path = write_policy(tmp.path(), FIXTURE_POLICY);

    let mut cmd = legion_cmd(tmp.path());
    cmd.arg("cmd-check")
        .arg("--hook")
        .env("LEGION_CMD_POLICY", &policy_path)
        .env_remove("CLAUDE_PLUGIN_ROOT");
    let out = run_with_stdin(&mut cmd, &hook_payload("Bash", "gh issue list"));

    assert!(out.status.success());
    let out_value = hook_specific(&out.stdout);
    assert_eq!(out_value["permissionDecision"], "allow");
    assert_eq!(out_value["updatedInput"]["command"], "legion issue list");
}

// -- Known gap (simplify finding, HIGH): a Rewrite replaces the WHOLE
// command string, so a compound command, a pipe, a redirect, or an extra
// flag on the matched invocation is silently dropped rather than refused.
// The fix belongs in `route` (#1228: a rewrite is lossless only for a
// single simple command whose args are all declared translatable); the
// adapter must not build that check itself (FR-CMD-014). These tests
// document today's actual (unsafe) behavior in their names and assert the
// DENY #1228 should produce instead -- `#[ignore]`d so they fail loudly
// and switch on the moment #1228 lands, rather than silently passing
// against behavior that was never fixed.

#[test]
#[ignore = "needs #1228's lossless-rewrite rule"]
fn a_compound_rewrite_target_denies_instead_of_dropping_the_other_command() {
    let tmp = TempDir::new().expect("tempdir");
    let policy_path = write_policy(tmp.path(), FIXTURE_POLICY);

    let mut cmd = legion_cmd(tmp.path());
    cmd.arg("cmd-check")
        .arg("--hook")
        .env("LEGION_CMD_POLICY", &policy_path)
        .env_remove("CLAUDE_PLUGIN_ROOT");
    // Today this silently rewrites to a bare `legion issue list`, dropping
    // the `cd x &&` entirely. #1228 should make `route` deny this instead.
    let out = run_with_stdin(&mut cmd, &hook_payload("Bash", "cd x && gh issue list"));

    assert!(out.status.success());
    let out_value = hook_specific(&out.stdout);
    assert_eq!(out_value["permissionDecision"], "deny");
}

#[test]
#[ignore = "needs #1228's lossless-rewrite rule"]
fn a_piped_rewrite_target_denies_instead_of_dropping_the_pipe() {
    let tmp = TempDir::new().expect("tempdir");
    let policy_path = write_policy(tmp.path(), FIXTURE_POLICY);

    let mut cmd = legion_cmd(tmp.path());
    cmd.arg("cmd-check")
        .arg("--hook")
        .env("LEGION_CMD_POLICY", &policy_path)
        .env_remove("CLAUDE_PLUGIN_ROOT");
    // Today this silently rewrites to a bare `legion issue list`, dropping
    // `| head` entirely.
    let out = run_with_stdin(&mut cmd, &hook_payload("Bash", "gh issue list | head"));

    assert!(out.status.success());
    let out_value = hook_specific(&out.stdout);
    assert_eq!(out_value["permissionDecision"], "deny");
}

#[test]
#[ignore = "needs #1228's lossless-rewrite rule"]
fn a_redirected_rewrite_target_denies_instead_of_dropping_the_redirect() {
    let tmp = TempDir::new().expect("tempdir");
    let policy_path = write_policy(tmp.path(), FIXTURE_POLICY);

    let mut cmd = legion_cmd(tmp.path());
    cmd.arg("cmd-check")
        .arg("--hook")
        .env("LEGION_CMD_POLICY", &policy_path)
        .env_remove("CLAUDE_PLUGIN_ROOT");
    // Today this silently rewrites to a bare `legion issue list`, dropping
    // `> out.txt` entirely -- the agent believes its output was captured.
    let out = run_with_stdin(&mut cmd, &hook_payload("Bash", "gh issue list > out.txt"));

    assert!(out.status.success());
    let out_value = hook_specific(&out.stdout);
    assert_eq!(out_value["permissionDecision"], "deny");
}

#[test]
#[ignore = "needs #1228's lossless-rewrite rule"]
fn a_rewrite_target_with_an_untranslated_flag_denies_instead_of_dropping_it() {
    let tmp = TempDir::new().expect("tempdir");
    let policy_path = write_policy(tmp.path(), FIXTURE_POLICY);

    let mut cmd = legion_cmd(tmp.path());
    cmd.arg("cmd-check")
        .arg("--hook")
        .env("LEGION_CMD_POLICY", &policy_path)
        .env_remove("CLAUDE_PLUGIN_ROOT");
    // Today this silently rewrites to a bare `legion issue list`, dropping
    // `--state open` -- the rewritten command lists every issue instead of
    // just the open ones the agent asked for. (Deliberately not a
    // slash-containing value like `--repo other/org`: that would trip
    // `build_replacement`'s unrelated path-facts refusal and pass today
    // for the wrong reason, masking the actual flag-dropping gap this
    // test exists to document.)
    let out = run_with_stdin(
        &mut cmd,
        &hook_payload("Bash", "gh issue list --state open"),
    );

    assert!(out.status.success());
    let out_value = hook_specific(&out.stdout);
    assert_eq!(out_value["permissionDecision"], "deny");
}

#[test]
fn an_unconfirmed_ask_is_refused_with_a_confirm_hint() {
    let tmp = TempDir::new().expect("tempdir");
    let policy_path = write_policy(tmp.path(), FIXTURE_POLICY);

    let mut cmd = legion_cmd(tmp.path());
    cmd.arg("cmd-check")
        .arg("--hook")
        .env("LEGION_CMD_POLICY", &policy_path)
        .env_remove("CLAUDE_PLUGIN_ROOT");
    let out = run_with_stdin(&mut cmd, &hook_payload("Bash", "gh pr merge 42 --admin"));

    assert!(out.status.success());
    let out_value = hook_specific(&out.stdout);
    assert_eq!(out_value["permissionDecision"], "deny");
    let reason = out_value["permissionDecisionReason"].as_str().unwrap();
    assert!(reason.contains("bypass branch protection?"));
    assert!(reason.contains("legion cmd confirm"));
}

#[test]
fn a_malformed_hook_payload_denies_never_allows() {
    let tmp = TempDir::new().expect("tempdir");
    let policy_path = write_policy(tmp.path(), FIXTURE_POLICY);

    let mut cmd = legion_cmd(tmp.path());
    cmd.arg("cmd-check")
        .arg("--hook")
        .env("LEGION_CMD_POLICY", &policy_path)
        .env_remove("CLAUDE_PLUGIN_ROOT");
    let out = run_with_stdin(&mut cmd, b"not json at all");

    assert!(out.status.success(), "always exits 0 even on bad input");
    let out_value = hook_specific(&out.stdout);
    assert_eq!(out_value["permissionDecision"], "deny");
}

#[test]
fn a_broken_policy_file_denies_never_allows() {
    let tmp = TempDir::new().expect("tempdir");
    let policy_path = write_policy(tmp.path(), "{ this is not valid json");

    let mut cmd = legion_cmd(tmp.path());
    cmd.arg("cmd-check")
        .arg("--hook")
        .env("LEGION_CMD_POLICY", &policy_path)
        .env_remove("CLAUDE_PLUGIN_ROOT");
    let out = run_with_stdin(&mut cmd, &hook_payload("Bash", "ls -la"));

    assert!(out.status.success());
    let out_value = hook_specific(&out.stdout);
    assert_eq!(out_value["permissionDecision"], "deny");
    let reason = out_value["permissionDecisionReason"].as_str().unwrap();
    assert!(reason.contains("legion-cmd adapter failed closed"));
}

#[test]
fn no_policy_configured_at_all_denies_never_allows() {
    let tmp = TempDir::new().expect("tempdir");

    let mut cmd = legion_cmd(tmp.path());
    cmd.arg("cmd-check")
        .arg("--hook")
        .env_remove("LEGION_CMD_POLICY")
        .env_remove("CLAUDE_PLUGIN_ROOT");
    let out = run_with_stdin(&mut cmd, &hook_payload("Bash", "ls -la"));

    assert!(out.status.success());
    let out_value = hook_specific(&out.stdout);
    assert_eq!(out_value["permissionDecision"], "deny");
}

/// A zero decision deadline can never be met -- `route` and the (empty,
/// here) lookup pre-pass together take some non-zero, if tiny, amount of
/// time on a freshly spawned thread, and `recv_timeout(Duration::ZERO)`
/// never blocks to wait for it. This is the deterministic way to exercise
/// the real overrun path end to end without racing a sleep against the
/// router's own (microsecond) speed.
#[test]
fn a_zero_deadline_always_overruns_and_denies() {
    let tmp = TempDir::new().expect("tempdir");
    let policy_path = write_policy(
        tmp.path(),
        r#"{"route": {"deadline_ms": 0}, "tools": {}, "sym_jobs": []}"#,
    );

    let mut cmd = legion_cmd(tmp.path());
    cmd.arg("cmd-check")
        .arg("--hook")
        .env("LEGION_CMD_POLICY", &policy_path)
        .env_remove("CLAUDE_PLUGIN_ROOT");
    let out = run_with_stdin(&mut cmd, &hook_payload("Bash", "ls -la"));

    assert!(out.status.success(), "always exits 0 even on overrun");
    let out_value = hook_specific(&out.stdout);
    assert_eq!(out_value["permissionDecision"], "deny");
    let reason = out_value["permissionDecisionReason"].as_str().unwrap();
    assert!(reason.contains("deadline"));
}

#[test]
fn cmd_check_without_hook_flag_is_not_implemented() {
    let tmp = TempDir::new().expect("tempdir");
    let mut cmd = legion_cmd(tmp.path());
    cmd.arg("cmd-check");
    let out = cmd.output().expect("failed to execute legion binary");
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("not implemented"));
}
