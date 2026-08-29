//! Shared temp-database helper for db domain tests (#609).
//!
//! Converges the scattered per-file `test_db` helpers (db.rs, documents.rs,
//! stats.rs, uncertainty/storage.rs) on one definition.

use super::Database;

/// Create a database backed by a fresh tempdir for testing.
///
/// The `TempDir` is deliberately leaked via `std::mem::forget` so the backing
/// directory outlives the returned `Database`. Dropping it here would unlink
/// the db/-wal/-shm files while the connection is live, leaving the database
/// working only through unlinked-open-fd semantics -- and any test that
/// reopens the path, opens a second connection, or checkpoints would fail
/// mysteriously. Tests that need to reopen the database by path must bind
/// their own tempdir instead of using this helper (see the v1 migration test
/// in `db/mod.rs`). The leak is bounded: one small directory per test, and
/// the OS reclaims the temp filesystem location later.
pub(crate) fn test_db() -> Database {
    let dir = tempfile::tempdir().unwrap();
    let db = Database::open(&dir.path().join("test.db")).unwrap();
    std::mem::forget(dir);
    db
}

/// Land a permissive, always-passing schema document declaring
/// `x-doc-type: <doc_type>` (#1062), so a unit test whose actual subject is
/// something other than schema conformance -- id generation, archiving,
/// listing, revision bookkeeping, criteria normalization -- can keep writing
/// whatever ad hoc payload it already used before write-time schema
/// enforcement landed.
///
/// Idempotent: a test helper (e.g. `cli/issue.rs`'s `seed_requirement`) may
/// call this once per document it creates on the SAME `db`, and
/// `schema_for_type` requires exactly one hot schema per type -- seeding a
/// second one would turn every later write of that type into a spurious
/// "ambiguous schema" failure. Checking `schema_for_type` first makes a
/// repeat call on an already-seeded db a no-op instead.
pub(crate) fn seed_type_schema(db: &Database, doc_type: &str) {
    if db.schema_for_type(doc_type).is_ok() {
        return;
    }
    // Built via `Map::insert` rather than the `json!` macro: the macro's key
    // position wants a literal, and `DOC_TYPE_KEYWORD` is a `const &str`
    // shared with the real validator so the two never drift apart.
    let mut schema_obj = serde_json::Map::new();
    schema_obj.insert(
        "$schema".to_string(),
        serde_json::Value::String("http://json-schema.org/draft-07/schema#".to_string()),
    );
    schema_obj.insert(
        "title".to_string(),
        serde_json::Value::String(format!("{doc_type} (test stub)")),
    );
    schema_obj.insert(
        "type".to_string(),
        serde_json::Value::String("object".to_string()),
    );
    schema_obj.insert(
        "properties".to_string(),
        serde_json::json!({"meta": {"type": "object"}}),
    );
    schema_obj.insert(
        crate::documents::DOC_TYPE_KEYWORD.to_string(),
        serde_json::Value::String(doc_type.to_string()),
    );
    let schema = serde_json::Value::Object(schema_obj).to_string();
    let meta = crate::documents::DocumentMeta {
        id: None,
        doc_type: "schema",
        surface: None,
        status: None,
        priority: None,
        owner: "legion-test",
    };
    db.insert_document(&meta, &schema)
        .expect("seed_type_schema: insert stub schema document");
}
