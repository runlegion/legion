//! Integration tests: post / bullpen / surface / signal / pending-replies.

use crate::common::*;

#[test]
fn post_and_bullpen_roundtrip() {
    let dir = tempfile::tempdir().unwrap();

    // Post a message
    let stdout = run_ok(legion_cmd(dir.path()).args([
        "post",
        "--repo",
        "kelex",
        "--text",
        "shared insight about schema parsing",
    ]));
    assert!(
        !stdout.trim().is_empty(),
        "expected ID on stdout, got nothing"
    );

    // Read the bullpen from a different repo
    let stdout = run_ok(legion_cmd(dir.path()).args(["bullpen", "--repo", "rafters"]));
    assert!(
        stdout.contains("[kelex]"),
        "expected repo attribution, got: {stdout}"
    );
    assert!(
        stdout.contains("shared insight about schema parsing"),
        "expected post text, got: {stdout}"
    );
    assert!(
        stdout.contains("[Legion] Bullpen"),
        "expected bullpen header, got: {stdout}"
    );
}

#[test]
fn bullpen_count_output() {
    let dir = tempfile::tempdir().unwrap();

    // Post two messages
    run_ok(legion_cmd(dir.path()).args([
        "post",
        "--repo",
        "kelex",
        "--text",
        "first shared thought",
    ]));

    run_ok(legion_cmd(dir.path()).args([
        "post",
        "--repo",
        "rafters",
        "--text",
        "second shared thought",
    ]));

    // Check count from a reader that has not read the bullpen
    let stdout = run_ok(legion_cmd(dir.path()).args(["bullpen", "--repo", "platform", "--count"]));
    assert!(
        stdout.contains("2 unread posts on the bullpen"),
        "expected unread count, got: {stdout}"
    );

    // Read the bullpen to mark as read
    run_ok(legion_cmd(dir.path()).args(["bullpen", "--repo", "platform"]));

    // Count should now be zero (no output)
    let stdout = run_ok(legion_cmd(dir.path()).args(["bullpen", "--repo", "platform", "--count"]));
    assert!(
        stdout.is_empty(),
        "expected no output for zero unread, got: {stdout}"
    );
}

#[test]
fn bullpen_count_includes_pending_tasks() {
    let dir = tempfile::tempdir().unwrap();

    // Post to bullpen
    run_ok(legion_cmd(dir.path()).args(["post", "--repo", "kelex", "--text", "a shared thought"]));

    // Create a pending task for the reader
    run_ok(legion_cmd(dir.path()).args([
        "task",
        "create",
        "--from",
        "kelex",
        "--to",
        "platform",
        "--text",
        "urgent work",
        "--priority",
        "high",
    ]));

    // Count should show both posts and tasks
    let stdout = run_ok(legion_cmd(dir.path()).args(["bullpen", "--repo", "platform", "--count"]));
    assert!(
        stdout.contains("1 unread posts, 1 pending tasks on the bullpen"),
        "expected combined count, got: {stdout}"
    );
}

#[test]
fn post_with_metadata_flags() {
    let dir = tempfile::tempdir().unwrap();

    let stdout = run_ok(legion_cmd(dir.path()).args([
        "post",
        "--repo",
        "rafters",
        "--text",
        "shared domain knowledge",
        "--domain",
        "auth",
        "--tags",
        "security,jwt",
    ]));
    assert!(
        !stdout.trim().is_empty(),
        "expected ID on stdout, got nothing"
    );

    // Verify it shows up on the bullpen
    let stdout = run_ok(legion_cmd(dir.path()).args(["bullpen", "--repo", "kelex"]));
    assert!(
        stdout.contains("shared domain knowledge"),
        "expected post on bullpen, got: {stdout}"
    );
}

#[test]
fn surface_shows_recent_posts() {
    let dir = tempfile::tempdir().unwrap();

    // Post to the bullpen
    run_ok(legion_cmd(dir.path()).args(["post", "--repo", "rafters", "--text", "synapse insight"]));

    // Surface for a different repo should show the post
    let stdout = run_ok(legion_cmd(dir.path()).args(["surface", "--repo", "kelex"]));
    assert!(
        stdout.contains("[Synapse]"),
        "expected synapse header, got: {stdout}"
    );
    assert!(
        stdout.contains("synapse insight"),
        "expected post in surface output, got: {stdout}"
    );
}

#[test]
fn surface_empty_database() {
    let dir = tempfile::tempdir().unwrap();

    // Need to initialize the DB first
    run_ok(legion_cmd(dir.path()).args(["reflect", "--repo", "test", "--text", "setup"]));

    // No bullpen posts, no high-value, no chains -- should be empty
    let stdout = run_ok(legion_cmd(dir.path()).args(["surface", "--repo", "kelex"]));
    assert!(
        stdout.is_empty(),
        "expected empty surface for no highlights, got: {stdout}"
    );
}

#[test]
fn bullpen_aliases_backward_compatible() {
    let dir = tempfile::tempdir().unwrap();

    // Seed a post
    run_ok(legion_cmd(dir.path()).args(["post", "--repo", "kelex", "--text", "alias test"]));

    // Old "board" alias still works
    run_ok(legion_cmd(dir.path()).args(["board", "--repo", "rafters"]));

    // Short "bp" alias works
    run_ok(legion_cmd(dir.path()).args(["bp", "--repo", "rafters"]));
}

#[test]
fn signal_command_posts_formatted_signal() {
    let dir = tempfile::tempdir().unwrap();

    run_ok(legion_cmd(dir.path()).args([
        "signal", "--repo", "kelex", "--to", "legion", "--verb", "review", "--status", "approved",
        "--note", "ship it",
    ]));

    // Verify signal appears on the bullpen
    let stdout = run_ok(legion_cmd(dir.path()).args(["bullpen", "--repo", "platform"]));
    assert!(
        stdout.contains("@legion"),
        "expected signal recipient on bullpen, got: {stdout}"
    );
    assert!(
        stdout.contains("review"),
        "expected signal verb on bullpen, got: {stdout}"
    );
}

#[test]
fn signal_with_details() {
    let dir = tempfile::tempdir().unwrap();

    run_ok(legion_cmd(dir.path()).args([
        "signal",
        "--repo",
        "kelex",
        "--to",
        "legion",
        "--verb",
        "review",
        "--status",
        "approved",
        "--details",
        "surface:cap-output,chain:confirmed",
    ]));
}

#[test]
fn bullpen_signals_filter() {
    let dir = tempfile::tempdir().unwrap();

    // Post a signal
    run_ok(legion_cmd(dir.path()).args([
        "signal", "--repo", "kelex", "--to", "legion", "--verb", "review", "--status", "approved",
    ]));

    // Post a musing
    run_ok(legion_cmd(dir.path()).args([
        "post",
        "--repo",
        "rafters",
        "--text",
        "deep thoughts about design patterns",
    ]));

    // --signals should show only the signal
    let stdout =
        run_ok(legion_cmd(dir.path()).args(["bullpen", "--repo", "platform", "--signals"]));
    assert!(stdout.contains("@legion"), "expected signal, got: {stdout}");
    assert!(
        !stdout.contains("deep thoughts"),
        "expected no musings in --signals, got: {stdout}"
    );

    // --musings should show only the musing
    let stdout = run_ok(legion_cmd(dir.path()).args(["bullpen", "--repo", "courses", "--musings"]));
    assert!(
        stdout.contains("deep thoughts"),
        "expected musing, got: {stdout}"
    );
    assert!(
        !stdout.contains("@legion"),
        "expected no signals in --musings, got: {stdout}"
    );
}

#[test]
fn pending_replies_emits_wake_prompt_for_request_signals() {
    let dir = tempfile::tempdir().unwrap();

    // smugglr asks platform to review an RFC. Under #404 the wake/reply gate
    // is verb-only, so the right shape is `--verb request` (not the pre-#404
    // `--verb review --status request` workaround).
    run_ok(legion_cmd(dir.path()).args([
        "signal",
        "--repo",
        "smugglr",
        "--to",
        "platform",
        "--verb",
        "request",
        "--note",
        "RFC review at vault-2026/projects/smuggler/fence/rfc.md",
    ]));

    // legion announces shipping -- informational, must NOT trip pending-replies.
    run_ok(legion_cmd(dir.path()).args([
        "signal",
        "--repo",
        "legion",
        "--to",
        "platform",
        "--verb",
        "announce",
        "--note",
        "v0.9.5 shipped",
    ]));

    let stdout = run_ok(legion_cmd(dir.path()).args(["pending-replies", "--repo", "platform"]));
    assert!(
        stdout.contains("REQUIRES A REPLY"),
        "verb=request must surface as reply-required, got: {stdout}"
    );
    assert!(
        stdout.contains("RFC review"),
        "expected the signal note in the prompt, got: {stdout}"
    );
    assert!(
        !stdout.contains("v0.9.5 shipped"),
        "informational announce must not appear in pending-replies, got: {stdout}"
    );
}

#[test]
fn pending_replies_silent_when_nothing_pending() {
    let dir = tempfile::tempdir().unwrap();

    run_ok(legion_cmd(dir.path()).args([
        "signal", "--repo", "legion", "--to", "platform", "--verb", "announce", "--note", "fyi",
    ]));

    let stdout = run_ok(legion_cmd(dir.path()).args(["pending-replies", "--repo", "platform"]));
    assert!(
        stdout.is_empty(),
        "pending-replies should print nothing when no reply-required signals exist, got: {stdout}"
    );
}
