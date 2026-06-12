//! Integration tests: spec-gen CLI (#527).
//!
//! Seeds service-design documents via `legion document create`, then runs
//! `legion spec-gen --repo <surface>` and asserts on created requirements
//! and kanban cards.

use crate::common::*;

/// Seed the requirement schema document (adopted ID).
/// Returns the schema doc id.
fn seed_requirement_schema(dir: &std::path::Path) -> String {
    let schema_payload = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/schemas/requirement.schema.json"
    ))
    .expect("requirement schema file");

    let out = run_with_stdin(
        legion_cmd(dir).args([
            "document",
            "create",
            "--doc-type",
            "schema",
            "--owner",
            "legion",
            "--id",
            "019eb45a-2fdd-7fd2-a574-96a3302bd15a",
            "--surface",
            "definition-layer",
            "--status",
            "adopted",
        ]),
        schema_payload.as_bytes(),
    );
    assert!(
        out.status.success(),
        "seed requirement schema failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// Seed an ecosystem document with 2 moments_of_truth on a surface.
fn seed_ecosystem(dir: &std::path::Path, surface: &str) -> String {
    let payload = serde_json::json!({
        "meta": {
            "title": "GitPress Ecosystem",
            "status": "done",
            "date": "2026-06-01",
            "author": "vault",
            "core_service": "gitpress"
        },
        "actors": {"primary": [{"name": "Developer", "role": "author"}]},
        "channels": [{"name": "web", "type": "digital", "purpose": "publishing"}],
        "value_exchanges": [{"from": "Developer", "to": "GitPress", "gives": "content", "gets": "exposure"}],
        "moments_of_truth": [
            {
                "number": 1,
                "title": "First Publish",
                "actor": "Developer",
                "success": "Post is live within 30s",
                "failure": "Post stuck in draft"
            },
            {
                "number": 2,
                "title": "Reader Discovery",
                "actor": "Reader",
                "success": "Finds the post via search",
                "failure": "Post never surfaces"
            }
        ]
    });
    let out = run_with_stdin(
        legion_cmd(dir).args([
            "document",
            "create",
            "--doc-type",
            "ecosystem",
            "--owner",
            "vault",
            "--surface",
            surface,
        ]),
        payload.to_string().as_bytes(),
    );
    assert!(
        out.status.success(),
        "seed ecosystem failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

#[test]
fn spec_gen_creates_requirements_and_cards() {
    let dir = tempfile::tempdir().unwrap();
    seed_requirement_schema(dir.path());
    seed_ecosystem(dir.path(), "gitpress");

    // Run spec-gen.
    let stdout = run_ok(legion_cmd(dir.path()).args(["spec-gen", "--repo", "gitpress"]));

    // Summary line must report 2 created, 0 skipped, 0 rejected.
    assert!(
        stdout.contains("created 2"),
        "expected 2 created requirements, got: {stdout}"
    );
    assert!(
        stdout.contains("skipped 0"),
        "expected 0 skipped, got: {stdout}"
    );
    assert!(
        stdout.contains("rejected 0"),
        "expected 0 rejected, got: {stdout}"
    );

    // Two requirement documents must now exist on the surface.
    let list_out = run_ok(legion_cmd(dir.path()).args([
        "document",
        "list",
        "--doc-type",
        "requirement",
        "--surface",
        "gitpress",
        "--json",
    ]));
    let docs: serde_json::Value = serde_json::from_str(list_out.trim()).unwrap();
    let arr = docs.as_array().expect("expected array");
    assert_eq!(arr.len(), 2, "expected 2 requirement documents");

    // Each requirement must carry a traces_to with #moment_of_truth.N.
    for doc in arr {
        let payload: serde_json::Value =
            serde_json::from_str(doc["payload"].as_str().unwrap()).unwrap();
        let traces_to = payload["traces_to"].as_str().unwrap_or("");
        assert!(
            traces_to.contains("#moment_of_truth."),
            "traces_to must embed moment number, got: {traces_to}"
        );
    }

    // Two born-Backlog kanban cards must exist for the surface.
    // Cards are born in Backlog status which is excluded from the default
    // working-set view; use --backlog to see the unconsented inbox.
    let kanban_out = run_ok(legion_cmd(dir.path()).args([
        "kanban",
        "list",
        "--repo",
        "gitpress",
        "--backlog",
        "--json",
    ]));
    let cards: Vec<serde_json::Value> = kanban_out
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).expect("valid card JSON"))
        .collect();
    // All cards in this fresh DB are spec-gen born-Backlog cards.
    // Verify status and that source_url points to one of the requirement doc ids.
    let req_doc_ids: std::collections::HashSet<String> = docs
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d["id"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(
        cards.len(),
        2,
        "expected 2 born-Backlog cards, got: {cards:?}"
    );
    for card in &cards {
        assert_eq!(
            card["status"].as_str(),
            Some("backlog"),
            "card must be born Backlog, got: {card}"
        );
        let source_url = card["source_url"].as_str().unwrap_or("");
        assert!(
            req_doc_ids.contains(source_url),
            "card source_url '{source_url}' must match a requirement doc id in {req_doc_ids:?}"
        );
    }
}

#[test]
fn spec_gen_idempotent_second_run_creates_nothing() {
    let dir = tempfile::tempdir().unwrap();
    seed_requirement_schema(dir.path());
    seed_ecosystem(dir.path(), "gitpress");

    // First run.
    run_ok(legion_cmd(dir.path()).args(["spec-gen", "--repo", "gitpress"]));

    // Second run: must report 0 created, 2 skipped.
    let stdout = run_ok(legion_cmd(dir.path()).args(["spec-gen", "--repo", "gitpress"]));
    assert!(
        stdout.contains("created 0"),
        "second run must create nothing, got: {stdout}"
    );
    assert!(
        stdout.contains("skipped 2"),
        "second run must skip 2 existing, got: {stdout}"
    );
}

#[test]
fn spec_gen_no_docs_prints_empty_surface_message() {
    let dir = tempfile::tempdir().unwrap();
    seed_requirement_schema(dir.path());

    // No service-design docs seeded.
    let stdout = run_ok(legion_cmd(dir.path()).args(["spec-gen", "--repo", "empty-surface"]));
    assert!(
        stdout.contains("no service-design documents found"),
        "must report empty surface, got: {stdout}"
    );
}
