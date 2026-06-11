//! Integration tests: uncertainty predictions: emit / witness / orphans / calibration.

use crate::common::*;

#[test]
fn uncertainty_emit_writes_prediction() {
    // #357: emit happy path. CLI returns {id, orphan_after} JSON;
    // a follow-up calibration query against the cohort is empty (no
    // snapshots until #359 daemon runs) but does not error.
    let dir = tempfile::tempdir().unwrap();
    let stdout = run_ok(legion_cmd(dir.path()).args([
        "uncertainty",
        "emit",
        "--surface",
        "legion.task",
        "--feature-key",
        "scip.refactor",
        "--input-fingerprint",
        "fp-test-1",
        "--model",
        "claude-opus-4-7",
        "--model-version",
        "4.7",
        "--claimed-confidence",
        "0.7",
        "--payload",
        r#"{"predicted_tokens":1500}"#,
        "--orphan-ttl-days",
        "30",
    ]));
    let parsed: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    assert!(parsed["id"].is_string());
    assert!(parsed["orphan_after"].is_string());
}

#[test]
fn uncertainty_emit_zero_ttl_means_no_orphan_window() {
    // --orphan-ttl-days 0 disables orphan sweep for that row.
    let dir = tempfile::tempdir().unwrap();
    let stdout = run_ok(legion_cmd(dir.path()).args([
        "uncertainty",
        "emit",
        "--surface",
        "legion.task",
        "--feature-key",
        "scip.refactor",
        "--input-fingerprint",
        "fp-test-2",
        "--model",
        "claude-opus-4-7",
        "--model-version",
        "4.7",
        "--claimed-confidence",
        "0.7",
        "--payload",
        r#"{}"#,
        "--orphan-ttl-days",
        "0",
    ]));
    let parsed: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    assert!(parsed["orphan_after"].is_null());
}

#[test]
fn uncertainty_emit_invalid_confidence_is_nonblocking() {
    // Emit is non-blocking by design: caller errors surface on stderr
    // but exit 0 so an upstream hook can never break the agent.
    let dir = tempfile::tempdir().unwrap();
    let stderr = run_ok_stderr(legion_cmd(dir.path()).args([
        "uncertainty",
        "emit",
        "--surface",
        "legion.task",
        "--feature-key",
        "scip.refactor",
        "--input-fingerprint",
        "fp-test-3",
        "--model",
        "claude-opus-4-7",
        "--model-version",
        "4.7",
        "--claimed-confidence",
        "1.5",
        "--payload",
        r#"{}"#,
    ]));
    assert!(
        stderr.contains("claimed_confidence"),
        "expected error on stderr, got: {stderr}"
    );
}

#[test]
fn uncertainty_witness_advances_state() {
    // Emit a prediction, witness it, then verify the row state advanced.
    let dir = tempfile::tempdir().unwrap();
    let emit = run_ok(legion_cmd(dir.path()).args([
        "uncertainty",
        "emit",
        "--surface",
        "legion.task",
        "--feature-key",
        "scip.refactor",
        "--input-fingerprint",
        "fp-w-1",
        "--model",
        "claude-opus-4-7",
        "--model-version",
        "4.7",
        "--claimed-confidence",
        "0.7",
        "--payload",
        r#"{"predicted_tokens":1000}"#,
    ]));
    let emit_out: serde_json::Value = serde_json::from_str(emit.trim()).unwrap();
    let id = emit_out["id"].as_str().unwrap();

    let stdout = run_ok(legion_cmd(dir.path()).args([
        "uncertainty",
        "witness",
        id,
        "--outcome-label",
        "shipped",
        "--outcome-correctness",
        "0.95",
        "--payload",
        r#"{"actual_tokens":950}"#,
    ]));
    let parsed: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    assert_eq!(parsed["state"], "witnessed");
    assert!(parsed["witnessed_at"].is_string());
}

#[test]
fn uncertainty_witness_missing_id_fails() {
    let dir = tempfile::tempdir().unwrap();
    let (_stdout, stderr) = run_fail(legion_cmd(dir.path()).args([
        "uncertainty",
        "witness",
        "does-not-exist",
        "--outcome-label",
        "shipped",
        "--outcome-correctness",
        "0.9",
    ]));
    assert!(
        stderr.contains("prediction not found") || stderr.contains("does-not-exist"),
        "expected not-found error, got: {stderr}"
    );
}

#[test]
fn uncertainty_orphans_empty_initially() {
    let dir = tempfile::tempdir().unwrap();
    let stdout = run_ok(legion_cmd(dir.path()).args(["uncertainty", "orphans", "--json"]));
    let parsed: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    let arr = parsed.as_array().unwrap();
    assert!(arr.is_empty());
}

#[test]
fn uncertainty_calibration_empty_initially() {
    let dir = tempfile::tempdir().unwrap();
    let stdout = run_ok(legion_cmd(dir.path()).args(["uncertainty", "calibration", "--json"]));
    let parsed: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    let arr = parsed.as_array().unwrap();
    assert!(arr.is_empty());
}

// Gated on #[cfg(unix)] because the test spawns bash to run the hook
// scripts directly. Same gate pattern other shell-script-bearing tests
// in this file use. Windows CI still runs every cargo-only test in the
// uncertainty suite; the hook scripts themselves are exercised on
// ubuntu / macos runners by both this test and plugin/hooks/test-*.sh.
#[cfg(unix)]
#[test]
fn uncertainty_emit_witness_end_to_end_via_hook_against_real_binary() {
    // End-to-end: run the auto-emit shell hook against the real legion
    // binary, then run the witness hook against the same DB. Verifies the
    // hook -> CLI -> DB path round-trips without stubs, so any shape drift
    // between the hook scripts and the CLI parser surfaces here.
    use std::process::Command;

    let dir = tempfile::tempdir().unwrap();
    let data_dir = dir.path();
    let state_dir = data_dir.join("state");
    std::fs::create_dir_all(&state_dir).unwrap();

    let emit_hook = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("plugin/hooks/uncertainty-emit-on-task.sh");
    let witness_hook = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("plugin/hooks/uncertainty-witness-on-completion.sh");
    let legion_bin = env!("CARGO_BIN_EXE_legion");

    // Drive emit-on-task via the actual hook script.
    let emit_out = run_with_stdin(
        Command::new("bash")
            .arg(&emit_hook)
            .env("LEGION_DATA_DIR", data_dir)
            .env("LEGION_BIN", legion_bin)
            .env("XDG_STATE_HOME", &state_dir),
        br#"{"session_id":"e2e","tool_name":"TaskCreate","tool_input":{"subject":"e2e probe task"},"tool_response":{"id":"task-e2e-001"}}"#,
    );
    let emit_stderr = String::from_utf8_lossy(&emit_out.stderr);
    assert!(
        emit_out.status.success(),
        "emit hook should always exit 0; stderr: {emit_stderr}"
    );

    // The mapping file should now have one row with a real UUIDv7
    // prediction id produced by the live emit verb.
    let map_path = state_dir.join("legion").join("uncertainty-tasks-e2e.jsonl");
    let map_contents = std::fs::read_to_string(&map_path).expect("mapping file written");
    let row: serde_json::Value =
        serde_json::from_str(map_contents.lines().next().unwrap()).unwrap();
    assert_eq!(row["task_id"], "task-e2e-001");
    let prediction_id = row["prediction_id"].as_str().unwrap().to_string();
    assert_eq!(
        uuid::Uuid::parse_str(&prediction_id)
            .unwrap()
            .get_version_num(),
        7
    );

    // Witness via the hook against the same DB.
    let witness_out = run_with_stdin(
        Command::new("bash")
            .arg(&witness_hook)
            .env("LEGION_DATA_DIR", data_dir)
            .env("LEGION_BIN", legion_bin)
            .env("XDG_STATE_HOME", &state_dir),
        br#"{"session_id":"e2e","tool_name":"TaskUpdate","tool_input":{"task_id":"task-e2e-001","status":"completed"}}"#,
    );
    let witness_stderr = String::from_utf8_lossy(&witness_out.stderr);
    assert!(
        witness_out.status.success(),
        "witness hook should exit 0; stderr: {witness_stderr}"
    );

    // Verify the row truly advanced past Emitted by attempting a second
    // witness through the CLI -- the state machine rejects Witnessed ->
    // Witnessed, so success here means the first witness landed.
    run_fail(legion_cmd(data_dir).args([
        "uncertainty",
        "witness",
        &prediction_id,
        "--outcome-label",
        "shipped",
        "--outcome-correctness",
        "0.5",
    ]));
}
