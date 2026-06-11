//! Integration tests: structured document store: create / view / list / archive / validate.

use crate::common::*;

#[test]
fn document_create_view_list_archive_roundtrip() {
    // #456: full lifecycle through the CLI. Verify a structured payload
    // round-trips through insert -> view -> list -> archive, with
    // archived rows excluded from default list and included with --archived.
    let dir = tempfile::tempdir().unwrap();

    // Create a typed-id requirement document with full meta + JSON payload.
    let payload = r#"{"meta":{"id":"FR-TEST-001","type":"requirement"},"title":"Integration test target","description":"Round-trip through the documents table CLI."}"#;
    let create_out = run_with_stdin(
        legion_cmd(dir.path()).args([
            "document",
            "create",
            "--doc-type",
            "requirement",
            "--owner",
            "mail",
            "--id",
            "FR-TEST-001",
            "--surface",
            "email",
            "--status",
            "specified",
            "--priority",
            "SHALL",
        ]),
        payload.as_bytes(),
    );
    let create_stderr = String::from_utf8_lossy(&create_out.stderr);
    assert!(
        create_out.status.success(),
        "document create failed: {create_stderr}"
    );
    let stdout = String::from_utf8(create_out.stdout).unwrap();
    assert!(
        stdout.trim() == "FR-TEST-001",
        "expected stdout to be the id, got: {stdout}"
    );

    // View as JSON; assert shape.
    let view = run_ok(legion_cmd(dir.path()).args(["document", "view", "FR-TEST-001", "--json"]));
    let view_json: serde_json::Value = serde_json::from_str(view.trim()).unwrap();
    assert_eq!(view_json["id"], "FR-TEST-001");
    assert_eq!(view_json["doc_type"], "requirement");
    assert_eq!(view_json["surface"], "email");
    assert_eq!(view_json["status"], "specified");
    assert_eq!(view_json["priority"], "SHALL");
    assert_eq!(view_json["owner"], "mail");

    // List filters by type.
    let list = run_ok(legion_cmd(dir.path()).args([
        "document",
        "list",
        "--doc-type",
        "requirement",
        "--json",
    ]));
    let docs: serde_json::Value = serde_json::from_str(list.trim()).unwrap();
    let arr = docs.as_array().unwrap();
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["id"], "FR-TEST-001");

    // Archive the row.
    run_ok(legion_cmd(dir.path()).args(["document", "archive", "FR-TEST-001"]));

    // Default list excludes archived.
    let list_hot = run_ok(legion_cmd(dir.path()).args(["document", "list", "--json"]));
    let hot_docs: serde_json::Value = serde_json::from_str(list_hot.trim()).unwrap();
    assert!(hot_docs.as_array().unwrap().is_empty());

    // --archived returns the archived row.
    let list_cold =
        run_ok(legion_cmd(dir.path()).args(["document", "list", "--archived", "--json"]));
    let cold_docs: serde_json::Value = serde_json::from_str(list_cold.trim()).unwrap();
    let cold_arr = cold_docs.as_array().unwrap();
    assert_eq!(cold_arr.len(), 1);
    assert_eq!(cold_arr[0]["id"], "FR-TEST-001");
    assert!(cold_arr[0]["archived_at"].is_string());
}

#[test]
fn document_create_rejects_malformed_payload() {
    // #456: payload must be valid JSON. Garbage payload should produce
    // a clear error, not land in the table.
    let dir = tempfile::tempdir().unwrap();
    let out = run_with_stdin(
        legion_cmd(dir.path()).args([
            "document",
            "create",
            "--doc-type",
            "requirement",
            "--owner",
            "mail",
            "--id",
            "FR-BAD-001",
        ]),
        b"this is not json",
    );
    assert!(!out.status.success(), "malformed payload should fail");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("not valid JSON"),
        "expected JSON error message, got: {stderr}"
    );
}

// --- Documents: schema landing + validation (#526) ---

fn schema_payload_persona() -> String {
    std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/schemas/persona.schema.json"
    ))
    .expect("persona schema file")
}

fn create_schema_document(dir: &std::path::Path, payload: &str) -> std::process::Output {
    run_with_stdin(
        legion_cmd(dir).args([
            "document",
            "create",
            "--doc-type",
            "schema",
            "--owner",
            "legion",
            "--surface",
            "definition-layer",
        ]),
        payload.as_bytes(),
    )
}

/// Create a schema document, require success, and return the new doc id.
fn create_schema_ok(dir: &std::path::Path, payload: &str) -> String {
    let out = create_schema_document(dir, payload);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "schema create failed: {stderr}");
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

#[test]
fn schema_document_create_writes_recall_pointer() {
    let dir = tempfile::tempdir().unwrap();
    let doc_id = create_schema_ok(dir.path(), &schema_payload_persona());
    assert!(!doc_id.is_empty());

    // The pointer reflection makes the schema recallable by domain (#526
    // option A): recall --domain schema surfaces it, citing the doc id.
    let stdout =
        run_ok(legion_cmd(dir.path()).args(["recall", "--repo", "legion", "--domain", "schema"]));
    assert!(
        stdout.contains("[SCHEMA] Persona"),
        "recall must surface the schema pointer, got: {stdout}"
    );
    assert!(
        stdout.contains(&doc_id),
        "pointer must cite the document id {doc_id}, got: {stdout}"
    );
}

#[test]
fn schema_document_malformed_rejected_at_create() {
    let dir = tempfile::tempdir().unwrap();
    // Valid JSON, invalid JSON Schema: no $schema, no properties.
    let out = create_schema_document(dir.path(), r#"{"title": "Broken", "type": "object"}"#);
    assert!(
        !out.status.success(),
        "malformed schema must be rejected at create"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("$schema"),
        "rejection must name the missing field, got: {stderr}"
    );

    // Nothing landed: the table holds no schema documents.
    let listed =
        run_ok(legion_cmd(dir.path()).args(["document", "list", "--doc-type", "schema", "--json"]));
    let docs: serde_json::Value = serde_json::from_str(listed.trim()).unwrap();
    assert_eq!(docs.as_array().map(Vec::len), Some(0));
}

#[test]
fn document_validate_accepts_real_persona_instance() {
    let dir = tempfile::tempdir().unwrap();
    let doc_id = create_schema_ok(dir.path(), &schema_payload_persona());

    // The fixture is a verbatim copy of a real vault-2026 instance --
    // the retroactive-validation AC, pinned in CI without a vault checkout.
    let fixture = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/gitpress-developer.persona.json"
    );
    let stdout = run_ok(legion_cmd(dir.path()).args([
        "document", "validate", "--schema", &doc_id, "--file", fixture,
    ]));
    assert!(stdout.contains("valid against schema"));
}

#[test]
fn document_validate_rejects_broken_instance_with_paths() {
    let dir = tempfile::tempdir().unwrap();
    let doc_id = create_schema_ok(dir.path(), &schema_payload_persona());

    // identity missing entirely; needs[0].priority outside the enum.
    let broken = r#"{
        "meta": {"title": "X", "status": "done", "date": "2026-06-10",
                 "author": "t", "set": "s", "actor": "a"},
        "behaviors": [],
        "frustrations": [],
        "needs": [{"text": "n", "priority": "MUST"}]
    }"#;
    let out = run_with_stdin(
        legion_cmd(dir.path()).args(["document", "validate", "--schema", &doc_id]),
        broken.as_bytes(),
    );
    assert!(
        !out.status.success(),
        "broken instance must fail validation"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("missing required property 'identity'"),
        "must report the missing block, got: {stderr}"
    );
    assert!(
        stderr.contains("$.needs[0].priority"),
        "must report the enum violation with its path, got: {stderr}"
    );
}

#[test]
fn document_validate_refuses_non_schema_document() {
    let dir = tempfile::tempdir().unwrap();
    // Land a plain (non-schema) document.
    let out = run_with_stdin(
        legion_cmd(dir.path()).args([
            "document",
            "create",
            "--doc-type",
            "note",
            "--owner",
            "legion",
        ]),
        b"{}",
    );
    assert!(out.status.success());
    let doc_id = String::from_utf8(out.stdout).unwrap().trim().to_string();

    let (_stdout, stderr) = run_fail(legion_cmd(dir.path()).args([
        "document",
        "validate",
        "--schema",
        &doc_id,
        "--file",
        "/dev/null",
    ]));
    assert!(
        stderr.contains("not 'schema'"),
        "must refuse a non-schema document as the schema argument"
    );
}
