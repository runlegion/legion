//! `legion cmd-check` through the real binary, both modes.
//!
//! - Hook mode, `--hook` (#1229): one payload on stdin, one hook response on
//!   stdout, exit 0 every time, and never an allow on an adapter failure.
//! - Operator mode (#1230): `-- '<command>'` or `--tool T --input <JSON>`
//!   reports route's decision without running the command. Every decision
//!   exits 0, deny included. A usage error (more than one word after `--`,
//!   invalid `--input`, an unknown `--tool`, no command) prints a `[legion]`
//!   error, exits 2, and routes nothing.

use crate::common::{legion_cmd, run_ok, run_with_stdin};
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
fn the_shipped_policy_rewrites_an_explore_spawn_to_the_legion_explorer() {
    // #1233: the no-harness-explore.sh case end to end -- route's rewrite
    // from the shipped policy, and the adapter's patched tool_input with
    // only subagent_type changed.
    let dir = tempfile::tempdir().expect("tempdir");
    let input = serde_json::json!({
        "tool_name": "Agent",
        "tool_input": {"subagent_type": "Explore", "prompt": "map the router", "description": "map"},
        "session_id": "s1",
        "cwd": "/tmp/legion-test",
        "tool_use_id": "t1"
    })
    .to_string()
    .into_bytes();
    let out = hook_output(dir.path(), &shipped_policy_path(), &input);
    assert_eq!(out["permissionDecision"], "allow");
    assert_eq!(
        out["updatedInput"],
        serde_json::json!({
            "subagent_type": "legion:legion-explore",
            "prompt": "map the router",
            "description": "map"
        })
    );
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
    // FR-CMD-016: the default allow reaches the agent through the real binary.
    let context = passed["additionalContext"].as_str().expect("default note");
    assert!(context.contains("default allow"), "got: {context}");
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

// -- the operator and scripting mode (#1230) ---------------------------------

/// Runs `legion cmd-check <args>` with the shipped policy passed by
/// `--policy`, requiring exit 0; returns stdout.
fn operator_output(data_dir: &std::path::Path, args: &[&str]) -> String {
    let policy = shipped_policy_path();
    let out = legion_cmd(data_dir)
        .args(["cmd-check", "--policy"])
        .arg(&policy)
        .args(args)
        .env_remove("LEGION_CMD_POLICY")
        .env_remove("CLAUDE_PLUGIN_ROOT")
        .output()
        .expect("legion runs");
    assert!(
        out.status.success(),
        "a decision exits 0\nstderr:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn cmd_check_reports_a_deny_with_its_reason_facts_and_elapsed_time() {
    let dir = tempfile::tempdir().expect("tempdir");
    let text = operator_output(dir.path(), &["--", "rm -rf build"]);
    assert!(text.contains("decision: deny"), "{text}");
    assert!(text.contains("unrecoverable"), "{text}");
    assert!(text.contains("instead:"), "{text}");
    assert!(text.contains("facts:"), "{text}");
    assert!(text.contains("elapsed:"), "{text}");
}

#[test]
fn cmd_check_json_reports_a_rewrite_with_its_built_replacement() {
    let dir = tempfile::tempdir().expect("tempdir");
    let stdout = operator_output(
        dir.path(),
        &[
            "--json",
            "--tool",
            "Agent",
            "--input",
            r#"{"subagent_type": "Explore", "prompt": "map the router"}"#,
        ],
    );
    let report: Value = serde_json::from_str(&stdout).expect("one JSON report");
    assert_eq!(report["decision"]["kind"], "rewrite");
    assert_eq!(report["decision"]["target"], "legion:legion-explore");
    assert_eq!(
        report["replacement"],
        serde_json::json!({"subagent_type": "legion:legion-explore", "prompt": "map the router"})
    );
    assert!(report["facts"].is_object());
    assert!(report["elapsed"].is_object());
}

#[test]
fn cmd_check_never_runs_the_command_it_checks() {
    let dir = tempfile::tempdir().expect("tempdir");
    let marker = dir.path().join("marker");
    let command = format!("touch {}", marker.display());
    let text = operator_output(dir.path(), &["--", &command]);
    assert!(text.contains("decision: allow"), "{text}");
    assert!(!marker.exists(), "cmd-check ran the command it checked");
}

#[test]
fn cmd_check_reports_an_unreadable_policy_as_a_deny_and_exits_0() {
    let dir = tempfile::tempdir().expect("tempdir");
    let missing = dir.path().join("no-such-policy.json");
    let out = legion_cmd(dir.path())
        .args(["cmd-check", "--json", "--policy"])
        .arg(&missing)
        .args(["--", "echo hi"])
        .output()
        .expect("legion runs");
    assert!(out.status.success(), "a deny is a decision, not a failure");
    let report: Value = serde_json::from_slice(&out.stdout).expect("JSON report");
    assert_eq!(report["decision"]["kind"], "deny");
    let reason = report["decision"]["reason"].as_str().expect("reason");
    assert!(reason.contains("policy:"), "got: {reason}");
    assert!(reason.contains("no-such-policy.json"), "got: {reason}");
}

#[test]
fn cmd_check_usage_errors_exit_non_zero_with_a_legion_prefix() {
    let dir = tempfile::tempdir().expect("tempdir");
    for args in [
        vec!["cmd-check", "--tool", "Bsh", "--", "ls"],
        vec!["cmd-check", "--tool", "Edit", "--input", "{ not json"],
        vec!["cmd-check"],
    ] {
        let out = legion_cmd(dir.path()).args(&args).output().expect("runs");
        assert!(!out.status.success(), "{args:?} must fail");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(stderr.starts_with("[legion]"), "{args:?} stderr: {stderr}");
        assert!(out.stdout.is_empty(), "{args:?} printed a report");
    }
}

#[test]
fn cmd_check_refuses_more_than_one_word_after_the_separator() {
    // The invoking shell has already removed the words' quoting, so the
    // command is refused rather than rebuilt: exit 2, the usage message, and
    // no report (nothing is routed).
    let dir = tempfile::tempdir().expect("tempdir");
    let out = legion_cmd(dir.path())
        .args(["cmd-check", "--policy"])
        .arg(shipped_policy_path())
        .args(["--", "arr[i[0]]=x", "rm", "-rf", "build"])
        .output()
        .expect("runs");
    assert_eq!(out.status.code(), Some(2));
    assert_eq!(
        String::from_utf8_lossy(&out.stderr).trim_end(),
        "[legion] error: pass the command as one quoted argument, or use --tool/--input"
    );
    assert!(out.stdout.is_empty(), "a refused command printed a report");
}

#[test]
fn one_quoted_argument_decides_the_same_as_the_same_string_via_input() {
    let dir = tempfile::tempdir().expect("tempdir");
    for command in [
        "arr[i[0]]=x rm -rf build",
        "FOO+='a b' rm -rf build",
        "git commit -m \"a; rm -rf x\"",
    ] {
        let positional: Value =
            serde_json::from_str(&operator_output(dir.path(), &["--json", "--", command]))
                .expect("JSON report");
        let input = serde_json::json!({ "command": command }).to_string();
        let typed: Value = serde_json::from_str(&operator_output(
            dir.path(),
            &["--json", "--tool", "Bash", "--input", &input],
        ))
        .expect("JSON report");
        assert_eq!(positional["decision"], typed["decision"], "{command}");
    }
}

#[test]
fn cmd_check_help_describes_both_modes_and_that_the_command_is_not_run() {
    let dir = tempfile::tempdir().expect("tempdir");
    let out = legion_cmd(dir.path())
        .args(["cmd-check", "--help"])
        .output()
        .expect("runs");
    assert!(out.status.success());
    let help = String::from_utf8_lossy(&out.stdout);
    assert!(help.contains("without running the command"), "{help}");
    assert!(help.contains("Operator and scripting mode"), "{help}");
    assert!(help.contains("Hook mode"), "{help}");
    assert!(help.contains("--input"), "{help}");
    assert!(help.contains("--policy"), "{help}");
    // The one-argument rule: the usage line names a single <COMMAND>, and
    // nothing in the help renders the positional as variadic.
    let usage = help
        .lines()
        .find(|line| line.starts_with("Usage:"))
        .expect("a usage line");
    assert_eq!(usage, "Usage: legion cmd-check [OPTIONS] [-- <COMMAND>]");
    assert!(!help.contains("COMMAND>..."), "{help}");
    assert!(!help.contains("[COMMAND]..."), "{help}");
}

#[test]
fn every_applied_rewrite_on_a_fresh_store_records_exactly_one_prediction() {
    // #1272: the hook opens the store for the emit while the witness pass
    // runs beside the decision. On a fresh, unmigrated store two connections
    // migrating at once collided and the emit was lost. Each trial is a new
    // store, a rewrite, and a witness pass that has a transcript to read.
    for trial in 0..8 {
        let dir = tempfile::tempdir().expect("tempdir");
        let state = tempfile::tempdir().expect("state tempdir");
        let transcript = dir.path().join("session.jsonl");
        std::fs::write(&transcript, "").expect("transcript");
        let input = serde_json::json!({
            "tool_name": "Agent",
            "tool_input": {"subagent_type": "Explore", "prompt": "map the router"},
            "session_id": format!("s{trial}"),
            "transcript_path": transcript,
            "cwd": "/tmp/legion-test",
            "tool_use_id": format!("toolu_{trial}")
        })
        .to_string()
        .into_bytes();
        let out = run_with_stdin(
            legion_cmd(dir.path())
                .args(["cmd-check", "--hook"])
                .env("LEGION_CMD_POLICY", shipped_policy_path())
                .env("XDG_STATE_HOME", state.path()),
            &input,
        );
        let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
        let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
        let response: Value = serde_json::from_str(stdout.trim()).expect("valid JSON response");
        assert_eq!(
            response["hookSpecificOutput"]["permissionDecision"], "allow",
            "trial {trial}: the rewrite must apply\nstderr:\n{stderr}"
        );

        let listed = run_ok(
            legion_cmd(dir.path())
                .args([
                    "uncertainty",
                    "predictions",
                    "--surface",
                    "legion.cmd",
                    "--json",
                ])
                .env("XDG_STATE_HOME", state.path()),
        );
        let rows: Vec<Value> = serde_json::from_str(listed.trim()).expect("predictions JSON");
        assert_eq!(
            rows.len(),
            1,
            "trial {trial}: expected one legion.cmd prediction, got {rows:?}\nhook stderr:\n{stderr}"
        );
    }
}

#[test]
fn concurrent_hook_processes_on_a_fresh_store_each_record_their_prediction() {
    // #1272 cross-process check: four `cmd-check --hook` processes at once
    // against one fresh store, ten rounds. Every process applies a rewrite
    // with its own tool_use_id and runs a witness pass; every round must
    // record exactly four legion.cmd predictions.
    const PROCESSES: usize = 4;
    const ROUNDS: usize = 10;
    for round in 0..ROUNDS {
        let dir = tempfile::tempdir().expect("tempdir");
        let state = tempfile::tempdir().expect("state tempdir");
        let transcript = dir.path().join("session.jsonl");
        std::fs::write(&transcript, "").expect("transcript");
        let children: Vec<_> = (0..PROCESSES)
            .map(|process| {
                let input = serde_json::json!({
                    "tool_name": "Agent",
                    "tool_input": {"subagent_type": "Explore", "prompt": "map the router"},
                    "session_id": format!("s{round}"),
                    "transcript_path": transcript,
                    "cwd": "/tmp/legion-test",
                    "tool_use_id": format!("toolu_{round}_{process}")
                })
                .to_string()
                .into_bytes();
                let mut cmd = legion_cmd(dir.path());
                cmd.args(["cmd-check", "--hook"])
                    .env("LEGION_CMD_POLICY", shipped_policy_path())
                    .env("XDG_STATE_HOME", state.path());
                std::thread::spawn(move || run_with_stdin(&mut cmd, &input))
            })
            .collect();
        let mut stderrs = String::new();
        for child in children {
            let out = child.join().expect("hook thread");
            let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
            stderrs.push_str(&String::from_utf8_lossy(&out.stderr));
            let response: Value = serde_json::from_str(stdout.trim()).expect("valid JSON response");
            assert_eq!(
                response["hookSpecificOutput"]["permissionDecision"], "allow",
                "round {round}: the rewrite must apply\nstderr:\n{stderrs}"
            );
        }

        let listed = run_ok(
            legion_cmd(dir.path())
                .args([
                    "uncertainty",
                    "predictions",
                    "--surface",
                    "legion.cmd",
                    "--json",
                ])
                .env("XDG_STATE_HOME", state.path()),
        );
        let rows: Vec<Value> = serde_json::from_str(listed.trim()).expect("predictions JSON");
        assert_eq!(
            rows.len(),
            PROCESSES,
            "round {round}: expected {PROCESSES} legion.cmd predictions, got {}\nhook stderr:\n{stderrs}",
            rows.len()
        );
    }
}
