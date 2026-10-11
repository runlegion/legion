//! Integration tests: mesh headroom / pick ranked from `host_status` rows.

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

/// Create the store, then write one `host_status` row stamped `age_secs`
/// ago -- the row a daemon (or gossip from a peer) would have written.
fn seed_host(data_dir: &std::path::Path, host: &str, cpu: f64, mem: f64, age_secs: i64) {
    // Any command opens the store and runs the schema chain.
    run_ok(legion_cmd(data_dir).args(["mesh", "headroom", "--json"]));
    let at = (chrono::Utc::now() - chrono::Duration::seconds(age_secs)).to_rfc3339();
    let conn = rusqlite::Connection::open(data_dir.join("legion.db")).expect("open store");
    conn.execute(
        "INSERT OR REPLACE INTO host_status \
         (hostname, cpu_pct, mem_pct, load_1m, agents_active, created_at, updated_at) \
         VALUES (?1, ?2, ?3, NULL, 1, ?4, ?4)",
        rusqlite::params![host, cpu, mem, at],
    )
    .expect("seed host_status");
}

#[test]
fn mesh_headroom_json_shape_pins_field_names() {
    let dir = tempfile::tempdir().unwrap();
    seed_host(dir.path(), "puck", 40.0, 55.0, 5);

    let stdout = run_ok(legion_cmd(dir.path()).args(["mesh", "headroom", "--json"]));
    let parsed: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON");
    let rows = parsed.as_array().expect("expected array");
    assert_eq!(rows.len(), 1);
    let row = &rows[0];
    // Contract: downstream schedulers / hooks will key on these names.
    for field in [
        "hostname",
        "score",
        "cpuPct",
        "memPct",
        "agentsActive",
        "updatedAt",
        "ageSecs",
        "stale",
    ] {
        assert!(
            row.get(field).is_some(),
            "missing field '{field}' in headroom JSON: {row}"
        );
    }
    assert_eq!(
        row["score"].as_f64(),
        Some(45.0),
        "score = 100 - max(cpu, mem)"
    );
}

#[test]
fn mesh_headroom_table_prints_load_columns() {
    let dir = tempfile::tempdir().unwrap();
    seed_host(dir.path(), "puck", 40.0, 55.0, 5);

    let stdout = run_ok(legion_cmd(dir.path()).args(["mesh", "headroom"]));
    for col in ["host", "score", "cpu%", "mem%", "agents", "age", "status"] {
        assert!(stdout.contains(col), "missing column '{col}': {stdout}");
    }
    assert!(
        stdout.contains("puck") && stdout.contains("fresh"),
        "{stdout}"
    );
}

#[test]
fn mesh_pick_returns_host_with_lowest_max_load() {
    let dir = tempfile::tempdir().unwrap();
    // max(cpu, mem): laptop 80, puck 50, kestrel 60 -- puck has the most room
    // even though laptop has the lowest CPU.
    seed_host(dir.path(), "laptop", 10.0, 80.0, 5);
    seed_host(dir.path(), "puck", 50.0, 30.0, 5);
    seed_host(dir.path(), "kestrel", 60.0, 20.0, 5);
    // A stale host with no load never wins.
    seed_host(dir.path(), "forgotten", 0.0, 0.0, 3600);

    let stdout = run_ok(legion_cmd(dir.path()).args(["mesh", "pick"]));
    assert_eq!(stdout.trim(), "puck");
}

#[test]
fn mesh_pick_exits_1_when_all_hosts_are_stale() {
    let dir = tempfile::tempdir().unwrap();
    seed_host(dir.path(), "puck", 10.0, 10.0, 3600);
    seed_host(dir.path(), "laptop", 20.0, 20.0, 7200);

    let out = legion_cmd(dir.path())
        .args(["mesh", "pick"])
        .output()
        .expect("run legion");
    assert_eq!(out.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("no fresh host"), "got: {stderr}");
}

#[test]
fn mesh_pick_json_shape_pins_field_names() {
    let dir = tempfile::tempdir().unwrap();
    seed_host(dir.path(), "puck", 40.0, 55.0, 5);

    let stdout =
        run_ok(legion_cmd(dir.path()).args(["mesh", "pick", "--json", "--for-task", "card-xyz"]));
    let parsed: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON");
    for field in ["hostname", "score", "forTask"] {
        assert!(
            parsed.get(field).is_some(),
            "missing field '{field}' in pick JSON: {parsed}"
        );
    }
    assert_eq!(parsed["forTask"].as_str(), Some("card-xyz"));
}

#[test]
fn mesh_pick_exclude_removes_named_host() {
    let dir = tempfile::tempdir().unwrap();
    seed_host(dir.path(), "puck", 10.0, 10.0, 5);

    let (_stdout, stderr) =
        run_fail(legion_cmd(dir.path()).args(["mesh", "pick", "--exclude", "puck"]));
    assert!(stderr.contains("no fresh host"), "got: {stderr}");
}

#[test]
fn mesh_stale_cutoff_env_override_is_respected() {
    let dir = tempfile::tempdir().unwrap();
    seed_host(dir.path(), "puck", 40.0, 55.0, 30);

    let (_stdout, stderr) = run_fail(
        legion_cmd(dir.path())
            .env("LEGION_MESH_STALE_SECS", "3")
            .args(["mesh", "pick"]),
    );
    assert!(stderr.contains("no fresh host"), "got: {stderr}");
}
