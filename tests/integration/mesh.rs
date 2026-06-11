//! Integration tests: mesh headroom / pick across rate-limit samples.

use crate::common::*;

#[test]
fn mesh_headroom_on_empty_store_notices_no_samples() {
    let dir = tempfile::tempdir().unwrap();
    let stderr = run_ok_stderr(legion_cmd(dir.path()).args(["mesh", "headroom"]));
    assert!(
        stderr.contains("no samples yet"),
        "empty-store notice expected, got stderr: {stderr}"
    );
}

#[test]
fn mesh_headroom_json_on_empty_store_returns_array() {
    let dir = tempfile::tempdir().unwrap();
    let stdout = run_ok(legion_cmd(dir.path()).args(["mesh", "headroom", "--json"]));
    let parsed: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON");
    assert!(parsed.is_array(), "expected JSON array, got {parsed}");
    assert_eq!(parsed.as_array().unwrap().len(), 0);
}

#[test]
fn mesh_pick_on_empty_store_exits_nonzero() {
    let dir = tempfile::tempdir().unwrap();
    let (_stdout, stderr) = run_fail(legion_cmd(dir.path()).args(["mesh", "pick"]));
    assert!(
        stderr.contains("no fresh host"),
        "error message must name the condition, got: {stderr}"
    );
    assert!(
        !stderr.contains("WorkSource"),
        "error must not surface as a WorkSource variant -- operators grep that token for real plugin failures, got: {stderr}"
    );
}

#[test]
fn mesh_headroom_json_shape_pins_field_names() {
    let dir = tempfile::tempdir().unwrap();
    seed_rate_limit_sample(dir.path(), 40.0, 55.0);

    let stdout = run_ok(legion_cmd(dir.path()).args(["mesh", "headroom", "--json"]));
    let parsed: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON");
    let rows = parsed.as_array().expect("expected array");
    assert!(
        !rows.is_empty(),
        "seed should produce at least one host row"
    );
    let row = &rows[0];
    // Contract: downstream schedulers / hooks will key on these names.
    // A typo or camelCase flip is a silent breakage otherwise.
    for field in [
        "hostname",
        "score",
        "fiveHourPct",
        "sevenDayPct",
        "lastEffectiveTokens",
        "sampledAt",
        "ageSecs",
        "stale",
    ] {
        assert!(
            row.get(field).is_some(),
            "missing field '{field}' in headroom JSON: {row}"
        );
    }
}

#[test]
fn mesh_pick_json_shape_pins_field_names() {
    let dir = tempfile::tempdir().unwrap();
    seed_rate_limit_sample(dir.path(), 40.0, 55.0);

    let stdout =
        run_ok(legion_cmd(dir.path()).args(["mesh", "pick", "--json", "--for-task", "card-xyz"]));
    let parsed: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON");
    for field in ["hostname", "score", "forTask"] {
        assert!(
            parsed.get(field).is_some(),
            "missing field '{field}' in pick JSON: {parsed}"
        );
    }
    assert_eq!(
        parsed["forTask"].as_str(),
        Some("card-xyz"),
        "--for-task must round-trip into the JSON payload"
    );
}

#[test]
fn mesh_pick_exclude_removes_named_host() {
    let dir = tempfile::tempdir().unwrap();
    seed_rate_limit_sample(dir.path(), 10.0, 10.0);
    // Figure out the host's name from headroom output.
    let head = run_ok(legion_cmd(dir.path()).args(["mesh", "headroom", "--json"]));
    let parsed: serde_json::Value = serde_json::from_str(&head).unwrap();
    let host = parsed[0]["hostname"].as_str().unwrap().to_string();

    // Excluding the only fresh host must exit 1 with "no fresh host".
    let (_stdout, stderr) =
        run_fail(legion_cmd(dir.path()).args(["mesh", "pick", "--exclude", &host]));
    assert!(
        stderr.contains("no fresh host"),
        "error must name condition, got: {stderr}"
    );
}

#[test]
fn mesh_stale_cutoff_env_override_is_respected() {
    // Default cutoff is 600s. Seed a sample, sleep so the sample is
    // definitely older than 3 seconds, then run pick with a 3-second
    // cutoff. Sample must register as stale; pick must fail.
    let dir = tempfile::tempdir().unwrap();
    seed_rate_limit_sample(dir.path(), 40.0, 55.0);
    std::thread::sleep(std::time::Duration::from_secs(4));

    let (_stdout, stderr) = run_fail(
        legion_cmd(dir.path())
            .env("LEGION_MESH_STALE_SECS", "3")
            .args(["mesh", "pick"]),
    );
    assert!(
        stderr.contains("no fresh host"),
        "stale env-override path must surface the same no-fresh-host error, got: {stderr}"
    );
}
