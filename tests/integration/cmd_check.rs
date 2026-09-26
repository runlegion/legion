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
use std::process::Command;
use std::time::{Duration, Instant};

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
            .env("LEGION_CMD_POLICY", policy)
            // The incident log (#1237) lives in legion's telemetry dir; keep
            // it inside the test's own directory.
            .env("XDG_STATE_HOME", data_dir),
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
fn a_missing_policy_file_still_refuses_and_records_a_no_go_command() {
    // FR-CMD-025: the built-in no-go list applies when the file is absent.
    let dir = tempfile::tempdir().expect("tempdir");
    let missing = dir.path().join("no-such-policy.json");
    let out = hook_output(dir.path(), &missing, &payload("rm -rf /"));
    assert_eq!(out["permissionDecision"], "deny");
    let reason = out["permissionDecisionReason"].as_str().expect("reason");
    assert!(
        reason.contains("none: this command never runs"),
        "got: {reason}"
    );
    let log = std::fs::read_to_string(dir.path().join("legion").join("cmd-incidents.jsonl"))
        .expect("incident log written");
    assert_eq!(log.lines().count(), 1);
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
fn cmd_confirm_refuses_more_than_one_word_after_the_separator() {
    // `legion cmd confirm` takes the command the same way (#1237): split
    // argv is refused rather than rebuilt -- exit 2, the usage message, and
    // nothing written: no store, no incident log, no confirmation.
    let data = tempfile::tempdir().expect("tempdir");
    let state = tempfile::tempdir().expect("tempdir");
    let out = legion_cmd(data.path())
        .env("XDG_STATE_HOME", state.path())
        .env("CLAUDE_CODE_SESSION_ID", "s1")
        .env_remove("LEGION_CMD_POLICY")
        .args(["cmd", "confirm", "--reason", "needed"])
        .args(["--", "arr[i[0]]=x", "rm", "-rf", "build"])
        .output()
        .expect("runs");
    assert_eq!(out.status.code(), Some(2));
    assert_eq!(
        String::from_utf8_lossy(&out.stderr).trim_end(),
        "[legion] error: pass the command as one quoted argument: \
         legion cmd confirm --reason <why> -- '<command>'"
    );
    assert!(
        out.stdout.is_empty(),
        "a refused command printed a confirmation"
    );
    let written: Vec<PathBuf> = [data.path(), state.path()]
        .iter()
        .flat_map(|dir| std::fs::read_dir(dir).expect("read dir"))
        .map(|entry| entry.expect("entry").path())
        .collect();
    assert!(written.is_empty(), "a refused confirm wrote {written:?}");
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

// -- the response is never held by the store (#1288) --------------------------

/// A rewrite payload: the shipped policy rewrites an Explore spawn, which
/// needs no lookup, so the only write it makes to the store is the
/// prediction; the decision only reads the session's confirmations (#1237).
fn explore_rewrite_payload(tool_use_id: &str) -> Vec<u8> {
    serde_json::json!({
        "tool_name": "Agent",
        "tool_input": {"subagent_type": "Explore", "prompt": "map the router"},
        "session_id": "s1",
        "cwd": "/tmp/legion-test",
        "tool_use_id": tool_use_id
    })
    .to_string()
    .into_bytes()
}

/// The shipped policy with `route.deadline_ms` set to `deadline_ms`, written
/// into `dir`; returns its path.
fn shipped_policy_with_deadline(dir: &std::path::Path, deadline_ms: u64) -> PathBuf {
    let text = std::fs::read_to_string(shipped_policy_path()).expect("shipped policy");
    let mut policy: Value = serde_json::from_str(&text).expect("shipped policy is JSON");
    policy["route"]["deadline_ms"] = Value::from(deadline_ms);
    let path = dir.join("policy.json");
    std::fs::write(&path, policy.to_string()).expect("policy written");
    path
}

/// The store file under a data dir.
fn store_path(data_dir: &std::path::Path) -> PathBuf {
    data_dir.join("legion.db")
}

/// How long a hook run took to put its response on stdout, and to exit.
struct TimedHook {
    response: Value,
    first_line: Duration,
    exited: Duration,
    stderr: String,
}

/// Runs `cmd-check --hook` and times the first stdout line separately from
/// the exit, which `run_with_stdin` cannot: it waits for the exit.
fn timed_hook(
    data_dir: &std::path::Path,
    state: &std::path::Path,
    policy: &std::path::Path,
    input: &[u8],
) -> TimedHook {
    use std::io::{BufRead, Read, Write};
    use std::process::Stdio;
    let mut child = legion_cmd(data_dir)
        .args(["cmd-check", "--hook"])
        .env("LEGION_CMD_POLICY", policy)
        .env("XDG_STATE_HOME", state)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("legion spawns");
    let started = Instant::now();
    child
        .stdin
        .take()
        .expect("piped stdin")
        .write_all(input)
        .expect("payload written");
    let mut stdout = std::io::BufReader::new(child.stdout.take().expect("piped stdout"));
    let mut line = String::new();
    stdout.read_line(&mut line).expect("a response line");
    let first_line: Duration = started.elapsed();
    let status = child.wait().expect("the hook exits");
    let exited: Duration = started.elapsed();
    let mut stderr = String::new();
    child
        .stderr
        .take()
        .expect("piped stderr")
        .read_to_string(&mut stderr)
        .expect("stderr read");
    assert!(status.success(), "the hook must exit 0\nstderr:\n{stderr}");
    let mut response: Value = serde_json::from_str(line.trim())
        .unwrap_or_else(|e| panic!("valid JSON response ({e}): {line:?}\nstderr:\n{stderr}"));
    TimedHook {
        response: response["hookSpecificOutput"].take(),
        first_line,
        exited,
        stderr,
    }
}

/// Runs the binary once before a timed run, so the timing measures the
/// adapter and not the platform's first launch of a freshly built binary
/// (seen at 1.5 s before `main` on macOS).
fn warm_binary() {
    run_ok(Command::new(env!("CARGO_BIN_EXE_legion")).arg("--version"));
}

/// The `legion.cmd` predictions recorded in `data_dir`'s store.
fn cmd_predictions(data_dir: &std::path::Path, state: &std::path::Path) -> Vec<Value> {
    let listed = run_ok(
        legion_cmd(data_dir)
            .args([
                "uncertainty",
                "predictions",
                "--surface",
                "legion.cmd",
                "--json",
            ])
            .env("XDG_STATE_HOME", state),
    );
    serde_json::from_str(listed.trim()).expect("predictions JSON")
}

#[test]
fn a_held_store_write_lock_never_delays_the_flushed_response() {
    // #1288: another connection holds the store's write lock, so the
    // prediction's insert waits out the store's 2 s busy timeout. The
    // response must already be on stdout by then: it is written and flushed
    // before the prediction is.
    let dir = tempfile::tempdir().expect("tempdir");
    let state = tempfile::tempdir().expect("state tempdir");
    // A stamped store: opening it takes no write lock, so the only write the
    // held lock can block is the prediction's.
    assert!(cmd_predictions(dir.path(), state.path()).is_empty());
    let lock = rusqlite::Connection::open(store_path(dir.path())).expect("open the store");
    lock.execute_batch("BEGIN IMMEDIATE")
        .expect("take the write lock");
    // The 1000 ms bound below cannot absorb the platform's first launch of
    // the binary; warm it here rather than rely on the call above.
    warm_binary();

    let run = timed_hook(
        dir.path(),
        state.path(),
        &shipped_policy_path(),
        &explore_rewrite_payload("toolu_locked"),
    );
    assert_eq!(
        run.response["permissionDecision"], "allow",
        "{}",
        run.stderr
    );
    assert!(run.response.get("updatedInput").is_some());
    // The lock really held the prediction: the process outlived the busy
    // timeout, and the insert failed on the lock.
    assert!(
        run.exited >= Duration::from_millis(1800),
        "the process exited after {:?}; the held lock did not block the prediction\nstderr:\n{}",
        run.exited,
        run.stderr
    );
    assert!(
        run.stderr.contains("rewrite prediction failed"),
        "stderr: {}",
        run.stderr
    );
    // ...and the response did not wait for it.
    assert!(
        run.first_line < Duration::from_millis(1000),
        "the response took {:?}, held by the prediction's write",
        run.first_line
    );
}

#[test]
fn a_store_open_that_cannot_finish_by_the_deadline_denies_naming_it_within_the_deadline() {
    // #1288, FR-CMD-009: a fresh, unmigrated store whose write lock another
    // connection holds; opening it waits on the lock for the store's 2 s
    // busy timeout. The adapter waits at most the route deadline for its one
    // open, then denies naming it: the decision cannot read confirmations
    // without the store, and it never waits on the lock a second time. No
    // prediction, no witness pass.
    let dir = tempfile::tempdir().expect("tempdir");
    let state = tempfile::tempdir().expect("state tempdir");
    let policy = shipped_policy_with_deadline(dir.path(), 300);
    warm_binary();
    let lock = rusqlite::Connection::open(store_path(dir.path())).expect("create the store");
    lock.execute_batch("BEGIN IMMEDIATE")
        .expect("take the write lock");

    let run = timed_hook(
        dir.path(),
        state.path(),
        &policy,
        &explore_rewrite_payload("toolu_unopened"),
    );
    assert_eq!(run.response["permissionDecision"], "deny", "{}", run.stderr);
    let reason: &str = run.response["permissionDecisionReason"]
        .as_str()
        .expect("a deny reason");
    assert!(
        reason.contains("confirmations: the store could not be opened within 300ms"),
        "{reason}"
    );
    assert!(run.response.get("updatedInput").is_none());
    // One deadline for the open, then the deny: never the 2 s busy timeout
    // a second open would wait out, and never a hang.
    assert!(
        run.first_line >= Duration::from_millis(300),
        "the response came after {:?}, before the open's deadline",
        run.first_line
    );
    assert!(
        run.first_line < Duration::from_millis(1000),
        "the response took {:?}; the store open held the decision past its deadline\nstderr:\n{}",
        run.first_line,
        run.stderr
    );
    assert!(
        run.exited < Duration::from_millis(1500),
        "the process ran {:?} after a deny",
        run.exited
    );

    lock.execute_batch("ROLLBACK").expect("release the lock");
    drop(lock);
    assert!(
        cmd_predictions(dir.path(), state.path()).is_empty(),
        "a call whose store open timed out recorded a prediction"
    );
}

#[test]
fn the_largest_configured_deadline_yields_the_routing_decision() {
    // #1288: the store open, the witness budget and the decision all run
    // under `route.deadline_ms`; the largest value the policy accepts is the
    // rewrite, not a panic deny.
    let dir = tempfile::tempdir().expect("tempdir");
    let state = tempfile::tempdir().expect("state tempdir");
    let policy = shipped_policy_with_deadline(dir.path(), u64::MAX);
    let run = timed_hook(
        dir.path(),
        state.path(),
        &policy,
        &explore_rewrite_payload("toolu_extreme"),
    );
    assert_eq!(
        run.response["permissionDecision"], "allow",
        "{}",
        run.stderr
    );
    assert!(run.response.get("updatedInput").is_some());
    assert!(!run.stderr.contains("panic"), "stderr: {}", run.stderr);
}

#[test]
fn an_unopenable_store_denies_naming_the_store_error_and_records_no_prediction() {
    // #1288, FR-CMD-009: LEGION_DATA_DIR names a regular file, so no store
    // can be opened under it. The decision cannot read confirmations without
    // the store, so it is a deny naming the store error, returned at once;
    // the prediction is skipped rather than attempted.
    let dir = tempfile::tempdir().expect("tempdir");
    let state = tempfile::tempdir().expect("state tempdir");
    let not_a_dir = dir.path().join("not-a-dir");
    std::fs::write(&not_a_dir, "a file, not a data dir").expect("file written");
    warm_binary();

    let run = timed_hook(
        &not_a_dir,
        state.path(),
        &shipped_policy_path(),
        &explore_rewrite_payload("toolu_unopenable"),
    );
    assert_eq!(run.response["permissionDecision"], "deny", "{}", run.stderr);
    let reason: &str = run.response["permissionDecisionReason"]
        .as_str()
        .expect("a deny reason");
    assert!(reason.contains("confirmations: IO error"), "{reason}");
    assert!(run.response.get("updatedInput").is_none());
    // The shipped deadline is 7000 ms; a store that fails to open costs
    // none of it.
    assert!(
        run.first_line < Duration::from_millis(1000),
        "the deny took {:?}",
        run.first_line
    );
    assert!(
        run.stderr.contains("store open failed"),
        "stderr: {}",
        run.stderr
    );
    assert!(
        !run.stderr.contains("rewrite prediction"),
        "the prediction was attempted without a store\nstderr: {}",
        run.stderr
    );
    assert_eq!(
        std::fs::read_to_string(&not_a_dir).expect("still a file"),
        "a file, not a data dir",
        "the unopenable path was changed"
    );
}

/// Runs `legion cmd confirm` in session `s1` against the shipped policy.
fn confirm(data_dir: &std::path::Path, args: &[&str]) -> std::process::Output {
    run_with_stdin(
        legion_cmd(data_dir)
            .args(["cmd", "confirm"])
            .args(args)
            .env("LEGION_CMD_POLICY", shipped_policy_path())
            .env("CLAUDE_CODE_SESSION_ID", "s1")
            .env("XDG_STATE_HOME", data_dir),
        b"",
    )
}

#[test]
fn a_confirmed_ask_runs_once_through_the_real_binary() {
    // #1237: the agent confirms the asked command with a reason; the next
    // attempt in the same session proceeds, and the one after is asked again.
    // The shipped curl rule needs the operator, so the confirmed attempt is
    // the harness permission prompt carrying the agent's reason.
    let dir = tempfile::tempdir().expect("tempdir");
    let out = confirm(
        dir.path(),
        &[
            "--reason",
            "fetching the release notes",
            "--",
            "curl https://example.com",
        ],
    );
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let first = hook_output(
        dir.path(),
        &shipped_policy_path(),
        &payload("curl  'https://example.com'"),
    );
    assert_eq!(first["permissionDecision"], "ask");
    assert_eq!(
        first["permissionDecisionReason"],
        "fetching the release notes"
    );
    let second = hook_output(
        dir.path(),
        &shipped_policy_path(),
        &payload("curl https://example.com"),
    );
    assert_eq!(second["permissionDecision"], "deny");
}

#[test]
fn confirm_refuses_a_missing_reason_and_a_no_go_command() {
    let dir = tempfile::tempdir().expect("tempdir");
    let missing = confirm(dir.path(), &["--", "curl https://example.com"]);
    assert!(!missing.status.success());
    assert!(String::from_utf8_lossy(&missing.stderr).contains("--reason is required"));

    let empty = confirm(
        dir.path(),
        &["--reason", "", "--", "curl https://example.com"],
    );
    assert!(!empty.status.success());

    let no_go = confirm(dir.path(), &["--reason", "cleanup", "--", "sudo rm -rf /"]);
    assert!(!no_go.status.success());
    assert!(String::from_utf8_lossy(&no_go.stderr).contains("no-go"));

    // Nothing was recorded: the command is still asked.
    let out = hook_output(
        dir.path(),
        &shipped_policy_path(),
        &payload("curl https://example.com"),
    );
    assert_eq!(out["permissionDecision"], "deny");
}

#[test]
fn a_no_go_command_is_refused_and_recorded_through_the_real_binary() {
    let dir = tempfile::tempdir().expect("tempdir");
    for command in ["rm -rf /", "sudo rm -rf /", "sh -c 'rm -rf /'"] {
        let out = hook_output(dir.path(), &shipped_policy_path(), &payload(command));
        assert_eq!(out["permissionDecision"], "deny", "{command}");
        let reason = out["permissionDecisionReason"].as_str().expect("reason");
        assert!(
            reason.contains("none: this command never runs"),
            "{command}: {reason}"
        );
        assert!(reason.contains("This attempt was recorded."), "{reason}");
    }
    let log = std::fs::read_to_string(dir.path().join("legion").join("cmd-incidents.jsonl"))
        .expect("incident log written under XDG_STATE_HOME");
    assert_eq!(log.lines().count(), 3);
    // The first hit sent the operator notice through the real signal path;
    // no send failure was recorded on its row.
    let first: Value = serde_json::from_str(log.lines().next().expect("a row")).expect("json");
    assert_eq!(first["hit_count"], 1);
    assert!(first["notice_error"].is_null(), "row: {first}");
}
