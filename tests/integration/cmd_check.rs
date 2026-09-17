//! Integration tests for `legion cmd-check --hook` (#1229): the real
//! subprocess, fed real hook JSON on stdin, reading a real policy file from
//! disk via `LEGION_CMD_POLICY`. Unlike `src/cmd/hook.rs`'s unit tests
//! (which call `build_response` in-process), these exercise the actual
//! binary boundary: process spawn, stdin/stdout plumbing, and exit code.

use crate::common::{legion_cmd, run_with_stdin};
use std::path::Path;
use std::process::Output;
use tempfile::TempDir;

fn write_policy(dir: &Path, contents: &str) -> std::path::PathBuf {
    let path = dir.join("policy.json");
    std::fs::write(&path, contents).expect("write fixture policy");
    path
}

/// Runs `legion cmd-check --hook` once with `payload` on stdin, in a fresh
/// tempdir. `policy` is written as `LEGION_CMD_POLICY` when given; `None`
/// leaves it (and `CLAUDE_PLUGIN_ROOT`) unset, exercising the
/// no-policy-configured path. Every `--hook` test in this file drives the
/// same binary boundary through this one helper so the process/env/stdin
/// wiring is written once.
fn run_hook_cli(policy: Option<&str>, payload: &[u8]) -> Output {
    let tmp = TempDir::new().expect("tempdir");
    let mut cmd = legion_cmd(tmp.path());
    cmd.arg("cmd-check")
        .arg("--hook")
        .env_remove("CLAUDE_PLUGIN_ROOT");
    match policy {
        Some(contents) => {
            let policy_path = write_policy(tmp.path(), contents);
            cmd.env("LEGION_CMD_POLICY", &policy_path);
        }
        None => {
            cmd.env_remove("LEGION_CMD_POLICY");
        }
    }
    run_with_stdin(&mut cmd, payload)
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
              "outcome": { "kind": "rewrite", "target": "legion issue list --repo {repo}",
                           "reason": "duplicate surface", "exact_args": ["issue", "list"] }
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
    let out = run_hook_cli(Some(FIXTURE_POLICY), &hook_payload("Bash", "ls -la"));

    assert!(out.status.success(), "run_hook always exits 0");
    let out_value = hook_specific(&out.stdout);
    assert!(out_value.get("permissionDecision").is_none());
}

#[test]
fn a_governed_command_denies_naming_reason_and_instead() {
    let out = run_hook_cli(Some(FIXTURE_POLICY), &hook_payload("Bash", "chmod 777 x"));

    assert!(out.status.success());
    let out_value = hook_specific(&out.stdout);
    assert_eq!(out_value["permissionDecision"], "deny");
    let reason = out_value["permissionDecisionReason"].as_str().unwrap();
    assert!(reason.contains("opens the tree to everyone"));
    assert!(reason.contains("chmod 755"));
}

#[test]
fn a_rewrite_returns_updated_input_and_allow() {
    let out = run_hook_cli(Some(FIXTURE_POLICY), &hook_payload("Bash", "gh issue list"));

    assert!(out.status.success());
    let out_value = hook_specific(&out.stdout);
    assert_eq!(out_value["permissionDecision"], "allow");
    // `cwd: "/repo/legion"` resolves to repo "legion" (repo_from_cwd),
    // substituted for the target's `{repo}` placeholder.
    assert_eq!(
        out_value["updatedInput"]["command"],
        "legion issue list --repo legion"
    );
}

// -- #1228's lossless-rewrite rule (route's own gate): a Rewrite replaces
// the WHOLE command string, so a compound command, a pipe, a redirect, or
// an extra/untranslated flag on the matched invocation would silently
// drop the rest of the command if route allowed the rewrite through.

#[test]
fn a_compound_rewrite_target_denies_instead_of_dropping_the_other_command() {
    // `cd x &&` makes this a compound command, not a single simple one;
    // `route`'s whole-command gate denies rather than rewriting and
    // dropping the `cd`.
    let out = run_hook_cli(
        Some(FIXTURE_POLICY),
        &hook_payload("Bash", "cd x && gh issue list"),
    );

    assert!(out.status.success());
    let out_value = hook_specific(&out.stdout);
    assert_eq!(out_value["permissionDecision"], "deny");
}

#[test]
fn a_piped_rewrite_target_denies_instead_of_dropping_the_pipe() {
    // `| head` makes this a compound command; denies rather than
    // rewriting and dropping the pipe.
    let out = run_hook_cli(
        Some(FIXTURE_POLICY),
        &hook_payload("Bash", "gh issue list | head"),
    );

    assert!(out.status.success());
    let out_value = hook_specific(&out.stdout);
    assert_eq!(out_value["permissionDecision"], "deny");
}

#[test]
fn a_redirected_rewrite_target_denies_instead_of_dropping_the_redirect() {
    // `> out.txt` makes this a compound command; denies rather than
    // rewriting and dropping the redirect (which would otherwise leave
    // the agent believing its output was captured).
    let out = run_hook_cli(
        Some(FIXTURE_POLICY),
        &hook_payload("Bash", "gh issue list > out.txt"),
    );

    assert!(out.status.success());
    let out_value = hook_specific(&out.stdout);
    assert_eq!(out_value["permissionDecision"], "deny");
}

#[test]
fn a_rewrite_target_with_an_untranslated_flag_denies_instead_of_dropping_it() {
    // `--state open` is not in the rule's declared `exact_args` (["issue",
    // "list"]), so `route`'s `first_differing_arg` check denies naming the
    // target rather than rewriting and dropping the flag (which would
    // otherwise list every issue instead of just the open ones).
    let out = run_hook_cli(
        Some(FIXTURE_POLICY),
        &hook_payload("Bash", "gh issue list --state open"),
    );

    assert!(out.status.success());
    let out_value = hook_specific(&out.stdout);
    assert_eq!(out_value["permissionDecision"], "deny");
}

#[test]
fn an_unconfirmed_ask_is_refused_with_a_confirm_hint() {
    let out = run_hook_cli(
        Some(FIXTURE_POLICY),
        &hook_payload("Bash", "gh pr merge 42 --admin"),
    );

    assert!(out.status.success());
    let out_value = hook_specific(&out.stdout);
    assert_eq!(out_value["permissionDecision"], "deny");
    let reason = out_value["permissionDecisionReason"].as_str().unwrap();
    assert!(reason.contains("bypass branch protection?"));
    assert!(reason.contains("legion cmd confirm"));
}

#[test]
fn a_malformed_hook_payload_denies_never_allows() {
    let out = run_hook_cli(Some(FIXTURE_POLICY), b"not json at all");

    assert!(out.status.success(), "always exits 0 even on bad input");
    let out_value = hook_specific(&out.stdout);
    assert_eq!(out_value["permissionDecision"], "deny");
}

#[test]
fn a_broken_policy_file_denies_never_allows() {
    let out = run_hook_cli(
        Some("{ this is not valid json"),
        &hook_payload("Bash", "ls -la"),
    );

    assert!(out.status.success());
    let out_value = hook_specific(&out.stdout);
    assert_eq!(out_value["permissionDecision"], "deny");
    let reason = out_value["permissionDecisionReason"].as_str().unwrap();
    assert!(reason.contains("legion-cmd adapter failed closed"));
}

#[test]
fn no_policy_configured_at_all_denies_never_allows() {
    let out = run_hook_cli(None, &hook_payload("Bash", "ls -la"));

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
    let out = run_hook_cli(
        Some(r#"{"route": {"deadline_ms": 0}, "tools": {}, "sym_jobs": []}"#),
        &hook_payload("Bash", "ls -la"),
    );

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
