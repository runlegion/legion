//! Integration tests: kanban CLI: create / list / work / lifecycle / view / update / reconcile.

use crate::common::*;

// --- Kanban CLI tests ---

#[test]
fn kanban_create_and_list() {
    let dir = tempfile::tempdir().unwrap();

    let id = run_ok(legion_cmd(dir.path()).args([
        "kanban",
        "create",
        "--from",
        "sean",
        "--to",
        "kelex",
        "--text",
        "implement search",
        "--priority",
        "high",
        "--labels",
        "backend,search",
    ]))
    .trim()
    .to_string();
    assert_eq!(id.len(), 36, "expected UUID, got: {id}");

    // --all so the freshly created (Backlog) card is visible; bare list is the
    // working set, which excludes Backlog (see kanban_list_scopes).
    let stdout =
        run_ok(legion_cmd(dir.path()).args(["kanban", "list", "--repo", "kelex", "--all"]));
    assert!(
        stdout.contains("implement search"),
        "expected card text, got: {stdout}"
    );
    assert!(
        stdout.contains("[high]"),
        "expected priority, got: {stdout}"
    );
    // born-Backlog: a newly created card lands in Backlog (AC #1 at the CLI boundary).
    assert!(
        stdout.contains("[backlog]"),
        "expected backlog status on a freshly created card, got: {stdout}"
    );
    // Labels are no longer shown inline -- they're stored but not displayed in list output
}

#[test]
fn kanban_list_scopes() {
    let dir = tempfile::tempdir().unwrap();

    // A raw Backlog card (created, never promoted).
    run_ok(legion_cmd(dir.path()).args([
        "kanban",
        "create",
        "--from",
        "sean",
        "--to",
        "kelex",
        "--text",
        "raw inbox card",
    ]));

    // A second card promoted to Pending via assign.
    let promoted_id = run_ok(legion_cmd(dir.path()).args([
        "kanban",
        "create",
        "--from",
        "sean",
        "--to",
        "kelex",
        "--text",
        "promoted card",
    ]))
    .trim()
    .to_string();
    run_ok(legion_cmd(dir.path()).args([
        "kanban",
        "assign",
        "--id",
        &promoted_id,
        "--to",
        "kelex",
    ]));

    // Bare list = working set: shows the promoted card, hides the raw Backlog one.
    let stdout = run_ok(legion_cmd(dir.path()).args(["kanban", "list", "--repo", "kelex"]));
    assert!(
        stdout.contains("promoted card"),
        "working set should show the promoted card, got: {stdout}"
    );
    assert!(
        !stdout.contains("raw inbox card"),
        "working set must hide Backlog cards, got: {stdout}"
    );

    // --backlog = only the raw inbox.
    let stdout =
        run_ok(legion_cmd(dir.path()).args(["kanban", "list", "--repo", "kelex", "--backlog"]));
    assert!(
        stdout.contains("raw inbox card"),
        "--backlog should show the Backlog card, got: {stdout}"
    );
    assert!(
        !stdout.contains("promoted card"),
        "--backlog must hide non-Backlog cards, got: {stdout}"
    );

    // --all = everything regardless of status.
    let stdout =
        run_ok(legion_cmd(dir.path()).args(["kanban", "list", "--repo", "kelex", "--all"]));
    assert!(
        stdout.contains("raw inbox card") && stdout.contains("promoted card"),
        "--all should show both cards, got: {stdout}"
    );
}

#[test]
fn kanban_work_picks_up_card() {
    let dir = tempfile::tempdir().unwrap();

    // Create two cards with different priorities
    let low_id = run_ok(legion_cmd(dir.path()).args([
        "kanban",
        "create",
        "--from",
        "sean",
        "--to",
        "kelex",
        "--text",
        "low prio",
        "--priority",
        "low",
    ]))
    .trim()
    .to_string();
    let high_id = run_ok(legion_cmd(dir.path()).args([
        "kanban",
        "create",
        "--from",
        "sean",
        "--to",
        "kelex",
        "--text",
        "high prio",
        "--priority",
        "high",
    ]))
    .trim()
    .to_string();

    // born-Backlog: promote both to Pending before work can pick them up.
    for id in [&low_id, &high_id] {
        run_ok(legion_cmd(dir.path()).args(["kanban", "assign", "--id", id, "--to", "kelex"]));
    }

    // Work should pick up the high priority card
    let stdout = run_ok(legion_cmd(dir.path()).args(["work", "--repo", "kelex"]));
    assert!(
        stdout.contains("high prio"),
        "expected high prio card, got: {stdout}"
    );
    assert!(
        stdout.contains("Priority: high"),
        "expected priority line, got: {stdout}"
    );
}

#[test]
fn kanban_work_empty_queue() {
    let dir = tempfile::tempdir().unwrap();

    // Init DB
    run_ok(legion_cmd(dir.path()).args(["reflect", "--repo", "test", "--text", "setup"]));

    let stderr =
        run_ok_stderr(legion_cmd(dir.path()).args(["--verbose", "work", "--repo", "kelex"]));
    assert!(
        stderr.contains("no pending work"),
        "expected no-work message, got: {stderr}"
    );
}

#[test]
fn kanban_work_peek_does_not_accept() {
    let dir = tempfile::tempdir().unwrap();

    let id = run_ok(legion_cmd(dir.path()).args([
        "kanban",
        "create",
        "--from",
        "sean",
        "--to",
        "kelex",
        "--text",
        "peek test",
        "--priority",
        "med",
    ]))
    .trim()
    .to_string();
    // born-Backlog: promote to Pending so work/peek can see it.
    run_ok(legion_cmd(dir.path()).args(["kanban", "assign", "--id", &id, "--to", "kelex"]));

    // Peek should show card but not accept
    let stdout = run_ok(legion_cmd(dir.path()).args(["work", "--repo", "kelex", "--peek"]));
    assert!(stdout.contains("peek test"), "expected card, got: {stdout}");

    // Card should still be pending (work without peek should still get it)
    let stdout = run_ok(legion_cmd(dir.path()).args(["work", "--repo", "kelex"]));
    assert!(
        stdout.contains("peek test"),
        "card should still be available, got: {stdout}"
    );
}

#[test]
fn kanban_full_lifecycle() {
    let dir = tempfile::tempdir().unwrap();

    // Create
    let id = run_ok(legion_cmd(dir.path()).args([
        "kanban",
        "create",
        "--from",
        "sean",
        "--to",
        "kelex",
        "--text",
        "lifecycle test",
    ]))
    .trim()
    .to_string();

    // born-Backlog: promote to Pending before accept.
    run_ok(legion_cmd(dir.path()).args(["kanban", "assign", "--id", &id, "--to", "kelex"]));

    // Accept
    run_ok(legion_cmd(dir.path()).args(["kanban", "accept", "--id", &id]));

    // Review
    run_ok(legion_cmd(dir.path()).args(["kanban", "review", "--id", &id]));

    // Done via kanban
    run_ok(
        legion_cmd(dir.path()).args(["done", "--repo", "kelex", "--text", "shipped", "--id", &id]),
    );
}

#[test]
fn kanban_block_unblock() {
    let dir = tempfile::tempdir().unwrap();

    let id = run_ok(legion_cmd(dir.path()).args([
        "kanban",
        "create",
        "--from",
        "sean",
        "--to",
        "kelex",
        "--text",
        "block test",
    ]))
    .trim()
    .to_string();

    // born-Backlog: promote to Pending before accept.
    run_ok(legion_cmd(dir.path()).args(["kanban", "assign", "--id", &id, "--to", "kelex"]));

    run_ok(legion_cmd(dir.path()).args(["kanban", "accept", "--id", &id]));

    run_ok(legion_cmd(dir.path()).args([
        "kanban",
        "block",
        "--id",
        &id,
        "--reason",
        "waiting on upstream",
    ]));

    run_ok(legion_cmd(dir.path()).args(["kanban", "unblock", "--id", &id]));
}

#[test]
fn kanban_invalid_transition() {
    let dir = tempfile::tempdir().unwrap();

    let id = run_ok(legion_cmd(dir.path()).args([
        "kanban",
        "create",
        "--from",
        "sean",
        "--to",
        "kelex",
        "--text",
        "invalid test",
    ]))
    .trim()
    .to_string();

    // Try to review a pending card (should fail -- must accept first)
    let (_stdout, stderr) =
        run_fail(legion_cmd(dir.path()).args(["kanban", "review", "--id", &id]));
    assert!(
        stderr.contains("InvalidCardTransition"),
        "expected transition error, got: {stderr}"
    );
}

#[test]
fn kanban_with_source_url() {
    let dir = tempfile::tempdir().unwrap();

    let id = run_ok(legion_cmd(dir.path()).args([
        "kanban",
        "create",
        "--from",
        "sean",
        "--to",
        "kelex",
        "--text",
        "github issue",
        "--source-url",
        "https://github.com/runlegion/legion/issues/42",
        "--source-type",
        "github",
    ]))
    .trim()
    .to_string();
    assert_eq!(id.len(), 36);

    let stdout =
        run_ok(legion_cmd(dir.path()).args(["kanban", "list", "--repo", "kelex", "--all"]));
    assert!(
        stdout.contains("github.com"),
        "expected source URL in output, got: {stdout}"
    );
}

// --- kanban view tests ---

#[test]
fn kanban_view_human_output() {
    let dir = tempfile::tempdir().unwrap();

    let id = run_ok(legion_cmd(dir.path()).args([
        "kanban",
        "create",
        "--from",
        "sean",
        "--to",
        "kelex",
        "--text",
        "view me",
        "--priority",
        "high",
        "--labels",
        "backend",
    ]))
    .trim()
    .to_string();

    let stdout = run_ok(legion_cmd(dir.path()).args(["kanban", "view", "--id", &id]));
    assert!(stdout.contains(&id), "card id in output");
    assert!(stdout.contains("view me"), "title in output");
    assert!(stdout.contains("high"), "priority in output");
}

#[test]
fn kanban_view_json_output() {
    let dir = tempfile::tempdir().unwrap();

    let id = run_ok(legion_cmd(dir.path()).args([
        "kanban",
        "create",
        "--from",
        "sean",
        "--to",
        "kelex",
        "--text",
        "json view",
    ]))
    .trim()
    .to_string();

    let stdout = run_ok(legion_cmd(dir.path()).args(["kanban", "view", "--id", &id, "--json"]));
    let parsed: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("output should be valid JSON");
    assert_eq!(parsed["id"].as_str().unwrap(), id, "id matches");
    assert_eq!(parsed["from_repo"].as_str().unwrap(), "sean");
}

#[test]
fn kanban_view_not_found_exits_nonzero() {
    let dir = tempfile::tempdir().unwrap();

    // Initialize DB
    run_ok(legion_cmd(dir.path()).args(["reflect", "--repo", "test", "--text", "setup"]));

    let (_stdout, stderr) = run_fail(legion_cmd(dir.path()).args([
        "kanban",
        "view",
        "--id",
        "00000000-0000-0000-0000-000000000000",
    ]));
    assert!(
        stderr.contains("card not found"),
        "expected card not found message, got: {stderr}"
    );
}

// --- kanban update tests ---

#[test]
fn kanban_update_text() {
    let dir = tempfile::tempdir().unwrap();

    let id = run_ok(legion_cmd(dir.path()).args([
        "kanban",
        "create",
        "--from",
        "sean",
        "--to",
        "kelex",
        "--text",
        "original title",
    ]))
    .trim()
    .to_string();

    // Should print the card id
    let stdout = run_ok(legion_cmd(dir.path()).args([
        "kanban",
        "update",
        "--id",
        &id,
        "--repo",
        "sean",
        "--text",
        "updated title",
    ]));
    assert!(stdout.trim() == id, "expected card id, got: {stdout}");

    // Verify the title changed
    let view = run_ok(legion_cmd(dir.path()).args(["kanban", "view", "--id", &id, "--json"]));
    let parsed: serde_json::Value = serde_json::from_str(view.trim()).expect("valid JSON");
    assert_eq!(parsed["text"].as_str().unwrap(), "updated title");
}

#[test]
fn kanban_update_priority() {
    let dir = tempfile::tempdir().unwrap();

    let id = run_ok(legion_cmd(dir.path()).args([
        "kanban",
        "create",
        "--from",
        "sean",
        "--to",
        "kelex",
        "--text",
        "prio test",
        "--priority",
        "low",
    ]))
    .trim()
    .to_string();

    run_ok(legion_cmd(dir.path()).args([
        "kanban",
        "update",
        "--id",
        &id,
        "--repo",
        "sean",
        "--priority",
        "critical",
    ]));

    let view = run_ok(legion_cmd(dir.path()).args(["kanban", "view", "--id", &id, "--json"]));
    let parsed: serde_json::Value = serde_json::from_str(view.trim()).expect("valid JSON");
    assert_eq!(parsed["priority"].as_str().unwrap(), "critical");
}

#[test]
fn kanban_update_add_labels_deduplicates() {
    let dir = tempfile::tempdir().unwrap();

    let id = run_ok(legion_cmd(dir.path()).args([
        "kanban",
        "create",
        "--from",
        "sean",
        "--to",
        "kelex",
        "--text",
        "label test",
        "--labels",
        "backend,api",
    ]))
    .trim()
    .to_string();

    run_ok(legion_cmd(dir.path()).args([
        "kanban",
        "update",
        "--id",
        &id,
        "--repo",
        "sean",
        "--add-labels",
        "api,frontend",
    ]));

    let view = run_ok(legion_cmd(dir.path()).args(["kanban", "view", "--id", &id, "--json"]));
    let parsed: serde_json::Value = serde_json::from_str(view.trim()).expect("valid JSON");
    let labels_raw = parsed["labels"].as_str().unwrap_or("");
    let label_parts: Vec<&str> = labels_raw.split(',').collect();
    assert_eq!(
        label_parts.iter().filter(|&&l| l == "api").count(),
        1,
        "api appears exactly once"
    );
    assert!(label_parts.contains(&"frontend"), "frontend added");
    assert!(label_parts.contains(&"backend"), "backend preserved");
}

#[test]
fn kanban_update_no_flags_exits_nonzero() {
    let dir = tempfile::tempdir().unwrap();

    let id = run_ok(legion_cmd(dir.path()).args([
        "kanban", "create", "--from", "sean", "--to", "kelex", "--text", "no flags",
    ]))
    .trim()
    .to_string();

    let (_stdout, stderr) =
        run_fail(legion_cmd(dir.path()).args(["kanban", "update", "--id", &id, "--repo", "sean"]));
    assert!(
        stderr.contains("no fields to update"),
        "expected helpful message, got: {stderr}"
    );
}

// --- kanban list --json tests ---

#[test]
fn kanban_list_json_emits_jsonl() {
    let dir = tempfile::tempdir().unwrap();

    run_ok(legion_cmd(dir.path()).args([
        "kanban",
        "create",
        "--from",
        "sean",
        "--to",
        "kelex",
        "--text",
        "## Card Alpha",
        "--labels",
        "backend",
    ]));
    run_ok(legion_cmd(dir.path()).args([
        "kanban",
        "create",
        "--from",
        "sean",
        "--to",
        "kelex",
        "--text",
        "Card Beta",
    ]));

    let stdout = run_ok(
        legion_cmd(dir.path()).args(["kanban", "list", "--repo", "kelex", "--all", "--json"]),
    );
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(lines.len(), 2, "two lines for two cards");

    for line in &lines {
        let parsed: serde_json::Value =
            serde_json::from_str(line).expect("each line should be valid JSON");
        assert!(parsed["id"].is_string(), "id present");
        assert!(parsed["title"].is_string(), "title present");
        assert!(parsed["status"].is_string(), "status present");
        assert!(
            !parsed.as_object().unwrap().contains_key("context"),
            "no context in summary"
        );
    }

    // Card Alpha should have heading stripped
    let alpha_line = lines.iter().find(|&&l| l.contains("Card Alpha"));
    assert!(
        alpha_line.is_some(),
        "Card Alpha (heading stripped) found in output"
    );
}

#[test]
fn kanban_list_json_labels_are_array() {
    let dir = tempfile::tempdir().unwrap();

    run_ok(legion_cmd(dir.path()).args([
        "kanban",
        "create",
        "--from",
        "sean",
        "--to",
        "kelex",
        "--text",
        "labeled",
        "--labels",
        "backend,api",
    ]));

    let stdout = run_ok(
        legion_cmd(dir.path()).args(["kanban", "list", "--repo", "kelex", "--all", "--json"]),
    );
    let line = stdout.lines().next().expect("has output");
    let parsed: serde_json::Value = serde_json::from_str(line).expect("valid JSON");
    let labels = parsed["labels"].as_array().expect("labels is array");
    assert!(labels.iter().any(|l| l.as_str() == Some("backend")));
    assert!(labels.iter().any(|l| l.as_str() == Some("api")));
}

#[test]
fn kanban_reconcile_apply_conflicts_with_close_stale() {
    // #444: --apply is sugar for both action flags; combining it with
    // either explicit flag is a parse error so the operator gets a
    // clear "pick one" message instead of double-applying.
    let dir = tempfile::tempdir().unwrap();
    run_fail(legion_cmd(dir.path()).args(["kanban", "reconcile", "--apply", "--close-stale"]));
}

#[test]
fn kanban_reconcile_apply_conflicts_with_cancel_shipped() {
    // #444: see kanban_reconcile_apply_conflicts_with_close_stale.
    let dir = tempfile::tempdir().unwrap();
    run_fail(legion_cmd(dir.path()).args(["kanban", "reconcile", "--apply", "--cancel-shipped"]));
}

#[test]
fn kanban_reconcile_dry_run_on_empty_db_is_quiet_success() {
    // #444: read-only mode on an empty DB exits 0 and emits both the
    // stale-open and shipped-pending empty-state lines so an operator
    // running this on a fresh node sees the empty-state report
    // explicitly rather than wondering whether the command did anything.
    let dir = tempfile::tempdir().unwrap();
    let stdout = run_ok(legion_cmd(dir.path()).args(["kanban", "reconcile"]));
    assert!(
        stdout.contains("no stale-open issues found"),
        "expected stale-open empty-state line, got: {stdout}"
    );
    assert!(
        stdout.contains("no shipped-pending cards found"),
        "expected shipped-pending empty-state line, got: {stdout}"
    );
}
