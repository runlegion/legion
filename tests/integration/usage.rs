//! Integration tests: usage reporting from Claude Code session JSONL files.

use crate::common::*;

// ---------------------------------------------------------------------------
// Usage command integration tests
// ---------------------------------------------------------------------------

/// Build a minimal assistant-turn JSONL event string for fixture files.
fn usage_assistant_event(
    input: u64,
    output: u64,
    cache_write: u64,
    cache_read: u64,
    ts: &str,
) -> String {
    format!(
        r#"{{"type":"assistant","timestamp":"{ts}","message":{{"role":"assistant","usage":{{"input_tokens":{input},"output_tokens":{output},"cache_creation_input_tokens":{cache_write},"cache_read_input_tokens":{cache_read}}}}}}}"#
    )
}

#[test]
fn usage_today_shows_table_header() {
    let data_dir = tempfile::tempdir().unwrap();
    let home_dir = tempfile::tempdir().unwrap();

    // Set up one session under a fake slug.
    let ts = "2099-01-01T00:00:00.000Z"; // far future -- always "today" test breaks if date matches, so use today
    let today = chrono::Utc::now()
        .format("%Y-%m-%dT00:01:00.000Z")
        .to_string();
    let slug = "-Users-test-projects-myrepo";
    let slug_dir = home_dir.path().join(".claude/projects").join(slug);
    std::fs::create_dir_all(&slug_dir).unwrap();
    let event = usage_assistant_event(100, 200, 0, 0, &today);
    std::fs::write(
        slug_dir.join("aaaaaaaa-0000-0000-0000-000000000001.jsonl"),
        format!("{event}\n"),
    )
    .unwrap();

    let _ = ts; // used for reference in comment above

    let stdout = run_ok(legion_cmd_with_home(data_dir.path(), home_dir.path()).args(["usage"]));
    // Table should contain "id" and "cost" column headers.
    assert!(
        stdout.contains("id") && stdout.contains("cost"),
        "expected table header in output, got: {stdout}"
    );
}

#[test]
fn usage_json_flag_emits_parseable_json() {
    let data_dir = tempfile::tempdir().unwrap();
    let home_dir = tempfile::tempdir().unwrap();

    let today = chrono::Utc::now()
        .format("%Y-%m-%dT00:02:00.000Z")
        .to_string();
    let slug = "-Users-test-projects-testjson";
    let slug_dir = home_dir.path().join(".claude/projects").join(slug);
    std::fs::create_dir_all(&slug_dir).unwrap();
    let event = usage_assistant_event(50, 100, 0, 0, &today);
    std::fs::write(
        slug_dir.join("bbbbbbbb-0000-0000-0000-000000000002.jsonl"),
        format!("{event}\n"),
    )
    .unwrap();

    let stdout = run_ok(
        legion_cmd_with_home(data_dir.path(), home_dir.path()).args(["usage", "--today", "--json"]),
    );
    // Must be valid JSON (parseable).
    let parsed: serde_json::Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("usage --json output is not valid JSON: {e}\noutput: {stdout}"));
    // Should be an array of sessions.
    assert!(parsed.is_array(), "expected JSON array, got: {parsed}");
}

#[test]
fn usage_by_repo_groups_sessions() {
    let data_dir = tempfile::tempdir().unwrap();
    let home_dir = tempfile::tempdir().unwrap();

    let today = chrono::Utc::now()
        .format("%Y-%m-%dT00:03:00.000Z")
        .to_string();
    let slug = "-Users-test-projects-groupedrepo";
    let slug_dir = home_dir.path().join(".claude/projects").join(slug);
    std::fs::create_dir_all(&slug_dir).unwrap();

    // Two sessions, same slug = same repo.
    let event = usage_assistant_event(10, 20, 0, 0, &today);
    std::fs::write(
        slug_dir.join("cccccccc-0000-0000-0000-000000000003.jsonl"),
        format!("{event}\n"),
    )
    .unwrap();
    std::fs::write(
        slug_dir.join("dddddddd-0000-0000-0000-000000000004.jsonl"),
        format!("{event}\n"),
    )
    .unwrap();

    let stdout = run_ok(
        legion_cmd_with_home(data_dir.path(), home_dir.path()).args([
            "usage",
            "--today",
            "--by-repo",
        ]),
    );
    // The grouped repo name should appear.
    assert!(
        stdout.contains("groupedrepo"),
        "expected repo name in --by-repo output, got: {stdout}"
    );
}

#[test]
fn usage_session_flag_exits_nonzero_when_not_found() {
    let data_dir = tempfile::tempdir().unwrap();
    let home_dir = tempfile::tempdir().unwrap();

    // Create an empty projects dir so discovery works.
    std::fs::create_dir_all(home_dir.path().join(".claude/projects")).unwrap();

    let (_stdout, stderr) = run_fail(
        legion_cmd_with_home(data_dir.path(), home_dir.path()).args([
            "usage",
            "--session",
            "nonexistent-uuid-1234",
        ]),
    );
    assert!(
        stderr.contains("session not found"),
        "expected 'session not found' error, got: {stderr}"
    );
}

#[test]
fn usage_no_sessions_prints_no_sessions_found() {
    let data_dir = tempfile::tempdir().unwrap();
    let home_dir = tempfile::tempdir().unwrap();

    // Create empty projects dir.
    std::fs::create_dir_all(home_dir.path().join(".claude/projects")).unwrap();

    let stdout =
        run_ok(legion_cmd_with_home(data_dir.path(), home_dir.path()).args(["usage", "--today"]));
    assert!(
        stdout.contains("no sessions found"),
        "expected 'no sessions found', got: {stdout}"
    );
}
