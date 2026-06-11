//! Integration tests: SCIP index status and doctrine-bypass telemetry.

use crate::common::*;

#[test]
fn index_status_empty_db() {
    // #284: --status on a fresh DB exits 0 and emits the no-indexes message.
    let dir = tempfile::tempdir().unwrap();
    let stderr = run_ok_stderr(legion_cmd(dir.path()).args(["--verbose", "index", "--status"]));
    assert!(
        stderr.contains("no SCIP indexes recorded"),
        "expected no-indexes message on stderr, got: {stderr}"
    );
}

#[test]
fn index_status_and_file_mutually_exclusive() {
    // #284: --status conflicts with --file.
    let dir = tempfile::tempdir().unwrap();
    run_fail(legion_cmd(dir.path()).args(["index", "--status", "--file", "/tmp/x"]));
}

#[test]
fn index_status_json_empty_db_returns_empty_array() {
    // #437: --json output is the contract `_legion-indexed.sh` (and
    // downstream #438/#439 hooks) read with `jq -e 'any(.[]; .repo == $r)'`.
    // The empty case must be a valid JSON array, not "null", not empty
    // stdout, not a wrapped object. Otherwise jq errors and every probe
    // silently degrades to "not indexed", disabling block-state.
    let dir = tempfile::tempdir().unwrap();
    let stdout = run_ok(legion_cmd(dir.path()).args(["index", "--status", "--json"]));
    let parsed: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("expected JSON array, got '{stdout}': {e}"));
    let arr = parsed
        .as_array()
        .unwrap_or_else(|| panic!("expected top-level array, got: {parsed}"));
    assert!(
        arr.is_empty(),
        "expected empty array on empty DB, got: {parsed}"
    );
}

#[test]
fn index_status_json_conflicts_with_banner() {
    // --json and --banner are mutually exclusive: banner is human-readable,
    // json is machine-readable. Combining them makes no sense and must fail
    // at parse time so an operator who tries the wrong combo gets a clear
    // error instead of unexpected output.
    let dir = tempfile::tempdir().unwrap();
    run_fail(legion_cmd(dir.path()).args(["index", "--status", "--json", "--banner", "anything"]));
}

#[test]
fn telemetry_record_and_list_roundtrip() {
    // #437: end-to-end CLI round-trip. Hooks (#438/#439) shell out with
    // long-form flags; if clap arg names or bool-flag semantics drift, the
    // hooks break silently. This pins the surface.
    let data_dir = tempfile::tempdir().unwrap();
    let xdg_state = tempfile::tempdir().unwrap();

    run_ok(
        legion_cmd(data_dir.path())
            .env("XDG_STATE_HOME", xdg_state.path())
            .args([
                "telemetry",
                "record-bypass",
                "--repo",
                "legion",
                "--session-id",
                "sess-int",
                "--tool",
                "Bash",
                "--pattern",
                "fn main",
                "--bypass-reason",
                "integration test",
                "--had-sym-hits",
                "--agent",
                "legion-prime",
            ]),
    );

    let stdout = run_ok(
        legion_cmd(data_dir.path())
            .env("XDG_STATE_HOME", xdg_state.path())
            .args(["telemetry", "list-bypasses", "--since", "1h"]),
    );
    let rows: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("expected JSON array, got '{stdout}': {e}"));
    let arr = rows
        .as_array()
        .unwrap_or_else(|| panic!("expected array, got: {rows}"));
    assert_eq!(arr.len(), 1, "expected one bypass row, got: {rows}");
    let row = &arr[0];
    assert_eq!(row["repo"], "legion");
    assert_eq!(row["tool"], "Bash");
    assert_eq!(row["pattern"], "fn main");
    assert_eq!(row["had_sym_hits"], true);
    assert_eq!(row["had_recall_hits"], false);
    assert_eq!(row["agent"], "legion-prime");
}

#[test]
fn telemetry_summary_rolls_up_groups() {
    // #440: summary groups by (tool, repo, pattern), sorts by count desc.
    // Seed three rows for one (Bash, legion, fn_main) group + one row for a
    // different (Read, legion, src/main.rs) group; assert top row is the
    // first group with count=3.
    let data_dir = tempfile::tempdir().unwrap();
    let xdg_state = tempfile::tempdir().unwrap();

    for _ in 0..3 {
        run_ok(
            legion_cmd(data_dir.path())
                .env("XDG_STATE_HOME", xdg_state.path())
                .args([
                    "telemetry",
                    "record-bypass",
                    "--repo",
                    "legion",
                    "--session-id",
                    "s",
                    "--tool",
                    "Bash",
                    "--pattern",
                    "fn_main",
                    "--bypass-reason",
                    "test",
                    "--had-sym-hits",
                ]),
        );
    }
    run_ok(
        legion_cmd(data_dir.path())
            .env("XDG_STATE_HOME", xdg_state.path())
            .args([
                "telemetry",
                "record-bypass",
                "--repo",
                "legion",
                "--session-id",
                "s",
                "--tool",
                "Read",
                "--pattern",
                "src/main.rs",
                "--bypass-reason",
                "test",
            ]),
    );

    let stdout = run_ok(
        legion_cmd(data_dir.path())
            .env("XDG_STATE_HOME", xdg_state.path())
            .args(["telemetry", "summary", "--since", "1h", "--json"]),
    );
    let rows: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    let arr = rows.as_array().unwrap();
    assert_eq!(arr.len(), 2, "two groups expected, got: {rows}");
    assert_eq!(arr[0]["tool"], "Bash");
    assert_eq!(arr[0]["count"], 3);
    assert!((arr[0]["had_sym_hits_pct"].as_f64().unwrap() - 1.0).abs() < 1e-9);
    assert_eq!(arr[1]["tool"], "Read");
    assert_eq!(arr[1]["count"], 1);
}

#[test]
fn telemetry_summary_empty_input_prints_no_bypasses_line() {
    // Human-readable output on empty bypass log emits a clear no-data
    // line so an operator running this on a fresh node sees the empty
    // state rather than nothing.
    let data_dir = tempfile::tempdir().unwrap();
    let xdg_state = tempfile::tempdir().unwrap();

    let stdout = run_ok(
        legion_cmd(data_dir.path())
            .env("XDG_STATE_HOME", xdg_state.path())
            .args(["telemetry", "summary"]),
    );
    assert!(
        stdout.contains("no bypasses recorded"),
        "expected no-bypasses line on empty log, got: {stdout}"
    );
}

#[test]
fn telemetry_list_filters_by_repo_and_since() {
    // Combined --since AND --repo filter: each is unit-tested alone in
    // src/telemetry.rs, but the CLI dispatch path that threads both into
    // list_bypasses is only exercised here.
    let data_dir = tempfile::tempdir().unwrap();
    let xdg_state = tempfile::tempdir().unwrap();

    for repo in ["legion", "smugglr", "legion"] {
        run_ok(
            legion_cmd(data_dir.path())
                .env("XDG_STATE_HOME", xdg_state.path())
                .args([
                    "telemetry",
                    "record-bypass",
                    "--repo",
                    repo,
                    "--session-id",
                    "sess",
                    "--tool",
                    "Bash",
                    "--pattern",
                    "x",
                    "--bypass-reason",
                    "test",
                ]),
        );
    }

    let stdout = run_ok(
        legion_cmd(data_dir.path())
            .env("XDG_STATE_HOME", xdg_state.path())
            .args([
                "telemetry",
                "list-bypasses",
                "--since",
                "1h",
                "--repo",
                "legion",
            ]),
    );
    let rows: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    let arr = rows.as_array().unwrap();
    assert_eq!(arr.len(), 2, "expected 2 legion rows, got: {rows}");
    for row in arr {
        assert_eq!(row["repo"], "legion");
    }
}
