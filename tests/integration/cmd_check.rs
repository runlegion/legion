//! CLI end-to-end tests for `legion cmd-check` (#1042, FR-CMD-008): the
//! sole CLI entry point into the legion-cmd router. The router's own
//! decision logic is unit-tested inside `crates/legion-cmd`; these tests
//! exercise the binary surface: argument parsing, the `--json` schema, and
//! the shared-entry-point property (a Bash command and a non-Bash tool call
//! both route, an unrecognized `--tool` is refused by name).

use crate::common::{legion_cmd, run_fail, run_ok};

#[test]
fn a_bash_command_prints_decision_and_targets_without_json() {
    let data_dir = tempfile::tempdir().expect("data dir");
    let stdout = run_ok(legion_cmd(data_dir.path()).args(["cmd-check", "ls -la"]));
    assert!(stdout.contains("decision: Allow"), "got:\n{stdout}");
    assert!(stdout.contains("targets.paths:"), "got:\n{stdout}");
    assert!(stdout.contains("targets.verb:"), "got:\n{stdout}");
    assert!(stdout.contains("targets.issues:"), "got:\n{stdout}");
    assert!(stdout.contains("targets.prs:"), "got:\n{stdout}");
    assert!(stdout.contains("targets.repo:"), "got:\n{stdout}");
    assert!(stdout.contains("targets.words:"), "got:\n{stdout}");
}

#[test]
fn tool_defaults_to_bash_when_omitted() {
    let data_dir = tempfile::tempdir().expect("data dir");
    let stdout = run_ok(legion_cmd(data_dir.path()).args(["cmd-check", "ls -la"]));
    // Same output whether --tool Bash is explicit or omitted -- both build
    // the identical ToolCall::Bash.
    let explicit =
        run_ok(legion_cmd(data_dir.path()).args(["cmd-check", "--tool", "Bash", "ls -la"]));
    assert_eq!(stdout, explicit);
}

#[test]
fn a_non_bash_tool_call_routes_via_json_input() {
    let data_dir = tempfile::tempdir().expect("data dir");
    let stdout = run_ok(legion_cmd(data_dir.path()).args([
        "cmd-check",
        "--tool",
        "Edit",
        r#"{"file_path": "a.rs", "new_string": "x"}"#,
    ]));
    assert!(stdout.contains("decision: Allow"), "got:\n{stdout}");
}

#[test]
fn an_unknown_tool_name_is_refused_and_lists_accepted_names() {
    let data_dir = tempfile::tempdir().expect("data dir");
    let (_stdout, stderr) = run_fail(legion_cmd(data_dir.path()).args([
        "cmd-check",
        "--tool",
        "Frobnicate",
        "whatever",
    ]));
    assert!(stderr.contains("Frobnicate"), "got:\n{stderr}");
    assert!(stderr.contains("Bash"), "got:\n{stderr}");
    assert!(stderr.contains("AskUserQuestion"), "got:\n{stderr}");
}

#[test]
fn malformed_json_for_a_non_bash_tool_fails_loudly_not_a_panic() {
    let data_dir = tempfile::tempdir().expect("data dir");
    let (_stdout, stderr) = run_fail(legion_cmd(data_dir.path()).args([
        "cmd-check",
        "--tool",
        "Edit",
        "{not valid json",
    ]));
    assert!(stderr.contains("JSON"), "got:\n{stderr}");
}

#[test]
fn json_output_matches_the_documented_cmd_check_output_schema() {
    let data_dir = tempfile::tempdir().expect("data dir");
    let stdout = run_ok(legion_cmd(data_dir.path()).args(["cmd-check", "--json", "ls -la"]));
    let value: serde_json::Value =
        serde_json::from_str(stdout.trim()).unwrap_or_else(|e| panic!("not JSON: {e}\n{stdout}"));
    let obj = value.as_object().expect("top-level object");
    for field in ["decision", "reason", "rewrite", "note", "targets"] {
        assert!(obj.contains_key(field), "missing field {field} in {stdout}");
    }
    assert_eq!(value["decision"], serde_json::json!("Allow"));
    assert_eq!(value["reason"], serde_json::json!(null));
    assert_eq!(value["note"], serde_json::json!(null));
    let targets = value["targets"].as_object().expect("targets object");
    for field in ["paths", "verb", "issues", "prs", "repo", "words"] {
        assert!(
            targets.contains_key(field),
            "missing targets.{field} in {stdout}"
        );
    }
}

#[test]
fn repo_flag_is_accepted_and_does_not_change_the_stub_router_decision() {
    // The embedded route table matches nothing yet (legion-cmd slice 1), so
    // --repo cannot yet influence the outcome -- this pins that --repo is
    // at least accepted and does not error, which is the CLI-surface
    // property this test owns.
    let data_dir = tempfile::tempdir().expect("data dir");
    let stdout = run_ok(legion_cmd(data_dir.path()).args([
        "cmd-check",
        "--repo",
        "legion",
        "--json",
        "ls -la",
    ]));
    let value: serde_json::Value = serde_json::from_str(stdout.trim()).expect("valid json");
    assert_eq!(value["decision"], serde_json::json!("Allow"));
}
