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

#[test]
fn uncertainty_roll_empty_db_is_noop() {
    // #359: `legion uncertainty roll` on a fresh DB must not error, and
    // must report all-zero counts rather than crash on the empty case.
    let dir = tempfile::tempdir().unwrap();
    let stdout = run_ok(legion_cmd(dir.path()).args(["uncertainty", "roll", "--json"]));
    let parsed: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    assert_eq!(parsed["predictions_swept"], 0);
    assert_eq!(parsed["cohorts_rolled"], 0);
    assert_eq!(parsed["buckets_written"], 0);
    assert_eq!(parsed["predictions_scored"], 0);
}

#[test]
fn uncertainty_roll_produces_a_calibration_snapshot() {
    // #359 end-to-end: emit -> witness -> roll -> calibration read shows
    // the freshly-rolled snapshot for that cohort.
    let dir = tempfile::tempdir().unwrap();
    let emit = run_ok(legion_cmd(dir.path()).args([
        "uncertainty",
        "emit",
        "--surface",
        "legion.roll-test",
        "--feature-key",
        "scip.refactor",
        "--input-fingerprint",
        "fp-roll-1",
        "--model",
        "claude-opus-4-7",
        "--model-version",
        "4.7",
        "--claimed-confidence",
        "0.9",
        "--payload",
        r#"{"predicted_tokens":1000}"#,
    ]));
    let emit_out: serde_json::Value = serde_json::from_str(emit.trim()).unwrap();
    let id = emit_out["id"].as_str().unwrap();

    run_ok(legion_cmd(dir.path()).args([
        "uncertainty",
        "witness",
        id,
        "--outcome-label",
        "shipped",
        "--outcome-correctness",
        "0.9",
    ]));

    let roll_stdout = run_ok(legion_cmd(dir.path()).args(["uncertainty", "roll", "--json"]));
    let roll_out: serde_json::Value = serde_json::from_str(roll_stdout.trim()).unwrap();
    assert_eq!(roll_out["cohorts_rolled"], 1);
    assert_eq!(roll_out["buckets_written"], 1);
    assert_eq!(roll_out["predictions_scored"], 1);

    let calibration_stdout = run_ok(legion_cmd(dir.path()).args([
        "uncertainty",
        "calibration",
        "--surface",
        "legion.roll-test",
        "--json",
    ]));
    let snaps: serde_json::Value = serde_json::from_str(calibration_stdout.trim()).unwrap();
    let arr = snaps.as_array().unwrap();
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["prediction_count"], 1);
    assert_eq!(arr[0]["brier_score"], 0.0);
}

#[test]
fn uncertainty_sweep_now_dispatches_to_sweep_orphans_only() {
    // #359 acceptance: "Both jobs runnable manually via `legion uncertainty
    // calibrate-now` / `legion uncertainty sweep-now` for testing" --
    // `sweep-now` must call ONLY Database::sweep_orphans, unlike `roll`
    // (sweep-then-roll under one shared `now`). The sweep SQL semantics
    // themselves (which rows actually flip to orphaned) are already
    // covered by uncertainty::storage::tests::
    // sweep_orphans_marks_only_emitted_past_orphan_after; this test proves
    // the CLI dispatch and the single-job isolation instead: a witnessed
    // prediction sitting ready to be rolled must NOT produce a calibration
    // snapshot as a side effect of running sweep-now.
    let dir = tempfile::tempdir().unwrap();
    let emit = run_ok(legion_cmd(dir.path()).args([
        "uncertainty",
        "emit",
        "--surface",
        "legion.sweep-now-test",
        "--feature-key",
        "scip.refactor",
        "--input-fingerprint",
        "fp-sweep-now-1",
        "--model",
        "claude-opus-4-7",
        "--model-version",
        "4.7",
        "--claimed-confidence",
        "0.9",
        "--payload",
        "{}",
    ]));
    let emit_out: serde_json::Value = serde_json::from_str(emit.trim()).unwrap();
    let id = emit_out["id"].as_str().unwrap();

    run_ok(legion_cmd(dir.path()).args([
        "uncertainty",
        "witness",
        id,
        "--outcome-label",
        "shipped",
        "--outcome-correctness",
        "0.9",
    ]));

    let stdout = run_ok(legion_cmd(dir.path()).args(["uncertainty", "sweep-now", "--json"]));
    let parsed: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    assert_eq!(parsed["predictions_swept"], 0);
    // Only key in the sweep-now JSON shape -- proves it does not also
    // report roll output.
    assert!(parsed.get("cohorts_rolled").is_none());

    let calibration_stdout = run_ok(legion_cmd(dir.path()).args([
        "uncertainty",
        "calibration",
        "--surface",
        "legion.sweep-now-test",
        "--json",
    ]));
    let snaps: serde_json::Value = serde_json::from_str(calibration_stdout.trim()).unwrap();
    assert!(
        snaps.as_array().unwrap().is_empty(),
        "sweep-now must not also roll calibration"
    );
}

#[test]
fn uncertainty_calibrate_now_dispatches_to_roll_calibration_only() {
    // #359 acceptance, other half: `calibrate-now` must call ONLY
    // Database::roll_calibration, not sweep first. Mirrors
    // `uncertainty_roll_produces_a_calibration_snapshot` but through the
    // single-job verb, proving the CLI dispatch and JSON shape
    // (cohorts_rolled/buckets_written/predictions_scored, no
    // predictions_swept key).
    let dir = tempfile::tempdir().unwrap();
    let emit = run_ok(legion_cmd(dir.path()).args([
        "uncertainty",
        "emit",
        "--surface",
        "legion.calibrate-now-test",
        "--feature-key",
        "scip.refactor",
        "--input-fingerprint",
        "fp-calibrate-now-1",
        "--model",
        "claude-opus-4-7",
        "--model-version",
        "4.7",
        "--claimed-confidence",
        "0.9",
        "--payload",
        "{}",
    ]));
    let emit_out: serde_json::Value = serde_json::from_str(emit.trim()).unwrap();
    let id = emit_out["id"].as_str().unwrap();

    run_ok(legion_cmd(dir.path()).args([
        "uncertainty",
        "witness",
        id,
        "--outcome-label",
        "shipped",
        "--outcome-correctness",
        "0.9",
    ]));

    let stdout = run_ok(legion_cmd(dir.path()).args(["uncertainty", "calibrate-now", "--json"]));
    let parsed: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    assert_eq!(parsed["cohorts_rolled"], 1);
    assert_eq!(parsed["buckets_written"], 1);
    assert_eq!(parsed["predictions_scored"], 1);
    assert!(parsed.get("predictions_swept").is_none());

    let calibration_stdout = run_ok(legion_cmd(dir.path()).args([
        "uncertainty",
        "calibration",
        "--surface",
        "legion.calibrate-now-test",
        "--json",
    ]));
    let snaps: serde_json::Value = serde_json::from_str(calibration_stdout.trim()).unwrap();
    let arr = snaps.as_array().unwrap();
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["prediction_count"], 1);
}

#[test]
fn uncertainty_roll_spread_produces_one_snapshot_row_per_bucket() {
    // #359 acceptance: "emit fake predictions across a cohort with a
    // confidence spread, witness them, run roll, assert snapshot rows
    // match expected buckets" -- driven through the real `legion
    // uncertainty` CLI surface (emit/witness/roll/calibration), not the
    // storage layer directly.
    //
    // `cohort_key` is itself derived from (surface, model, model_version,
    // confidence-decile) at emit time (`uncertainty::types::cohort_key`),
    // so three predictions spread across three different deciles land in
    // THREE cohorts, not one -- each contributing exactly one bucket to
    // this surface's reliability diagram. That is the real shape #359's
    // daemon cron will see in production, so `cohorts_rolled` and
    // `buckets_written` are equal here (3 and 3), unlike the storage-layer
    // test below which forces multiple buckets into a single cohort to
    // exercise `roll_calibration`'s internal fan-out directly.
    //
    // Three buckets, two predictions each: enough to prove `roll` emits
    // multiple distinct snapshot rows end-to-end without the process-spawn
    // cost of a ~100-prediction run through the CLI (100 emits + 100
    // witnesses = 200 spawned binaries). The exhaustive 100-prediction /
    // all-ten-bucket bucket-math case lives at the storage layer
    // (`uncertainty::storage::tests::roll_calibration_spread_across_all_ten_buckets`),
    // where it runs in milliseconds against an in-memory db.
    let dir = tempfile::tempdir().unwrap();
    let surface = "legion.spread-test";

    // (claimed_confidence, outcome_correctness) pairs, two per bucket, for
    // buckets 1 ([0.1,0.2)), 4 ([0.4,0.5)), and 8 ([0.8,0.9)).
    let cases: &[(f64, f64)] = &[
        (0.15, 0.1),
        (0.18, 0.2),
        (0.45, 0.5),
        (0.48, 0.4),
        (0.85, 0.9),
        (0.88, 0.8),
    ];

    for (i, (claimed, outcome)) in cases.iter().enumerate() {
        let fingerprint = format!("fp-spread-{i}");
        let emit = run_ok(legion_cmd(dir.path()).args([
            "uncertainty",
            "emit",
            "--surface",
            surface,
            "--feature-key",
            "scip.refactor",
            "--input-fingerprint",
            &fingerprint,
            "--model",
            "claude-opus-4-7",
            "--model-version",
            "4.7",
            "--claimed-confidence",
            &claimed.to_string(),
            "--payload",
            "{}",
        ]));
        let emit_out: serde_json::Value = serde_json::from_str(emit.trim()).unwrap();
        let id = emit_out["id"].as_str().unwrap().to_string();

        run_ok(legion_cmd(dir.path()).args([
            "uncertainty",
            "witness",
            &id,
            "--outcome-label",
            "shipped",
            "--outcome-correctness",
            &outcome.to_string(),
        ]));
    }

    let roll_stdout = run_ok(legion_cmd(dir.path()).args(["uncertainty", "roll", "--json"]));
    let roll_out: serde_json::Value = serde_json::from_str(roll_stdout.trim()).unwrap();
    // Three deciles touched -> three distinct cohort_keys -> three cohorts,
    // each rolling exactly one bucket.
    assert_eq!(roll_out["cohorts_rolled"], 3);
    assert_eq!(roll_out["buckets_written"], 3);
    assert_eq!(roll_out["predictions_scored"], 6);

    let calibration_stdout = run_ok(legion_cmd(dir.path()).args([
        "uncertainty",
        "calibration",
        "--surface",
        surface,
        "--json",
    ]));
    let snaps: serde_json::Value = serde_json::from_str(calibration_stdout.trim()).unwrap();
    let arr = snaps.as_array().unwrap();
    assert_eq!(arr.len(), 3, "expected one row per populated bucket");

    // list_calibration_snapshots orders by bucket_lower ASC.
    let expected_lowers = [0.1, 0.4, 0.8];
    let expected_uppers = [0.2, 0.5, 0.9];
    for (row, (expected_lower, expected_upper)) in arr
        .iter()
        .zip(expected_lowers.iter().zip(expected_uppers.iter()))
    {
        assert_eq!(row["prediction_count"], 2);
        let lower = row["bucket_lower"].as_f64().unwrap();
        let upper = row["bucket_upper"].as_f64().unwrap();
        assert!(
            (lower - expected_lower).abs() < 1e-9,
            "bucket_lower {lower} should be {expected_lower}"
        );
        assert!(
            (upper - expected_upper).abs() < 1e-9,
            "bucket_upper {upper} should be {expected_upper}"
        );
    }
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
    // The hooks source lib/prelude.sh via ${CLAUDE_PLUGIN_ROOT}/hooks/...
    // and fail open without it (#614); point at the in-tree plugin root.
    let plugin_root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("plugin");

    // Drive emit-on-task via the actual hook script.
    let emit_out = run_with_stdin(
        Command::new("bash")
            .arg(&emit_hook)
            .env("LEGION_DATA_DIR", data_dir)
            .env("LEGION_BIN", legion_bin)
            .env("CLAUDE_PLUGIN_ROOT", &plugin_root)
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
            .env("CLAUDE_PLUGIN_ROOT", &plugin_root)
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
