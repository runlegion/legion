//! Integration tests: task lifecycle and state machine.

use crate::common::*;

#[test]
fn task_full_lifecycle() {
    let dir = tempfile::tempdir().unwrap();

    // Create a task
    let task_id = run_ok(legion_cmd(dir.path()).args([
        "task",
        "create",
        "--from",
        "kelex",
        "--to",
        "legion",
        "--text",
        "implement search feature",
        "--priority",
        "high",
        "--context",
        "BM25 index needed",
    ]))
    .trim()
    .to_string();
    assert!(!task_id.is_empty(), "expected task ID on stdout");

    // List inbound tasks for legion
    let stdout = run_ok(legion_cmd(dir.path()).args(["task", "list", "--repo", "legion"]));
    assert!(
        stdout.contains("implement search feature"),
        "expected task text in list, got: {stdout}"
    );
    assert!(
        stdout.contains("[pending]"),
        "expected pending status, got: {stdout}"
    );
    assert!(
        stdout.contains("from:kelex"),
        "expected from attribution, got: {stdout}"
    );
    assert!(
        stdout.contains("[high]"),
        "expected high priority, got: {stdout}"
    );

    // Accept the task
    let stderr = run_ok_stderr(legion_cmd(dir.path()).args([
        "--verbose",
        "task",
        "accept",
        "--id",
        &task_id,
    ]));
    assert!(
        stderr.contains("task accepted"),
        "expected accept confirmation, got: {stderr}"
    );

    // Complete the task
    let stderr = run_ok_stderr(legion_cmd(dir.path()).args([
        "--verbose",
        "task",
        "done",
        "--id",
        &task_id,
        "--note",
        "shipped in v0.2",
    ]));
    assert!(
        stderr.contains("task completed"),
        "expected completion confirmation, got: {stderr}"
    );

    // List should show done status
    let stdout = run_ok(legion_cmd(dir.path()).args(["task", "list", "--repo", "legion"]));
    assert!(
        stdout.contains("[done]"),
        "expected done status, got: {stdout}"
    );
}

#[test]
fn task_block_flow() {
    let dir = tempfile::tempdir().unwrap();

    // Create and accept
    let task_id = run_ok(legion_cmd(dir.path()).args([
        "task",
        "create",
        "--from",
        "kelex",
        "--to",
        "legion",
        "--text",
        "blocked task",
    ]))
    .trim()
    .to_string();

    run_ok(legion_cmd(dir.path()).args(["task", "accept", "--id", &task_id]));

    // Block the task
    let stderr = run_ok_stderr(legion_cmd(dir.path()).args([
        "--verbose",
        "task",
        "block",
        "--id",
        &task_id,
        "--reason",
        "waiting on upstream",
    ]));
    assert!(
        stderr.contains("task blocked"),
        "expected block confirmation, got: {stderr}"
    );
}

#[test]
fn task_list_outbound() {
    let dir = tempfile::tempdir().unwrap();

    // Create tasks from kelex
    run_ok(legion_cmd(dir.path()).args([
        "task",
        "create",
        "--from",
        "kelex",
        "--to",
        "legion",
        "--text",
        "task for legion",
    ]));

    run_ok(legion_cmd(dir.path()).args([
        "task",
        "create",
        "--from",
        "kelex",
        "--to",
        "rafters",
        "--text",
        "task for rafters",
    ]));

    // List outbound from kelex
    let stdout = run_ok(legion_cmd(dir.path()).args(["task", "list", "--repo", "kelex", "--from"]));
    assert!(
        stdout.contains("task for legion"),
        "expected legion task, got: {stdout}"
    );
    assert!(
        stdout.contains("task for rafters"),
        "expected rafters task, got: {stdout}"
    );
    assert!(
        stdout.contains("outbound"),
        "expected outbound label, got: {stdout}"
    );
}

#[test]
fn task_invalid_state_transition() {
    let dir = tempfile::tempdir().unwrap();

    // Create a task
    let task_id = run_ok(legion_cmd(dir.path()).args([
        "task",
        "create",
        "--from",
        "kelex",
        "--to",
        "legion",
        "--text",
        "cannot skip accept",
    ]))
    .trim()
    .to_string();

    // Try to complete a pending task (should fail)
    let (_stdout, stderr) =
        run_fail(legion_cmd(dir.path()).args(["task", "done", "--id", &task_id]));
    assert!(
        stderr.contains("invalid state transition"),
        "expected state transition error, got: {stderr}"
    );
}

#[test]
fn task_surface_shows_pending() {
    let dir = tempfile::tempdir().unwrap();

    // Need to init DB first
    run_ok(legion_cmd(dir.path()).args(["reflect", "--repo", "test", "--text", "setup"]));

    // Create a pending task for legion
    run_ok(legion_cmd(dir.path()).args([
        "task",
        "create",
        "--from",
        "kelex",
        "--to",
        "legion",
        "--text",
        "pending task for surface",
    ]));

    // Surface should show the pending task
    let stdout = run_ok(legion_cmd(dir.path()).args(["surface", "--repo", "legion"]));
    assert!(
        stdout.contains("pending task for surface"),
        "expected pending task in surface, got: {stdout}"
    );
    assert!(
        stdout.contains("Task from kelex"),
        "expected task attribution, got: {stdout}"
    );
}
