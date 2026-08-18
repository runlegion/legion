//! Documents table -- the coordination substrate (#456, child of #455).
//!
//! Stores spec / NFR / blueprint / persona / journey / etc as rows with
//! a JSON payload + indexed meta columns hoisted from the payload's
//! `meta` block. Hot pool by default; `archived_at` populates when work
//! referencing the doc completes.
//!
//! The substrate is type-agnostic at the storage layer: any document
//! type whose payload carries the canonical `meta` shape lands here.
//! Type-specific schema validation belongs in a sibling issue once
//! vault ships the schemas.

use chrono::Utc;
use rusqlite::params;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::db::Database;
use crate::error::{LegionError, Result};

/// A document row. The `payload` field is the raw validated JSON; the
/// indexed columns are hoisted from `payload.meta` at insert time so
/// SQL queries can filter without parsing JSON.
///
/// `id` is sourced from `payload.meta.id` when present (typed ids like
/// `FR-EMAIL-003`), or generated as a UUIDv7 otherwise. Caller-supplied
/// ids must be globally unique across the table; collision is a hard
/// error.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Document {
    pub id: String,
    pub doc_type: String,
    pub surface: Option<String>,
    pub status: String,
    pub priority: Option<String>,
    pub owner: String,
    /// Raw payload JSON. Callers parse as needed for the specific type.
    pub payload: String,
    pub archived_at: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

/// Fields hoisted from `payload.meta` at insert time. Provided by the
/// caller; the storage layer does not parse the payload itself, keeping
/// this module type-agnostic. Schema validation lives in a sibling
/// child issue under #455.
#[derive(Debug, Clone)]
pub struct DocumentMeta<'a> {
    /// Optional caller-supplied id. When None, a UUIDv7 is generated.
    pub id: Option<&'a str>,
    pub doc_type: &'a str,
    pub surface: Option<&'a str>,
    /// Initial lifecycle status. Defaults to "draft" when None.
    pub status: Option<&'a str>,
    pub priority: Option<&'a str>,
    pub owner: &'a str,
}

/// Filter for `list_documents`. None on every field returns all rows.
/// Empty struct (all None) is the default broad query.
#[derive(Debug, Clone, Default)]
pub struct DocumentFilter<'a> {
    pub doc_type: Option<&'a str>,
    pub surface: Option<&'a str>,
    pub status: Option<&'a str>,
    pub owner: Option<&'a str>,
    /// When None, returns hot rows only. Some(true) returns archived only.
    /// Some(false) returns hot only (explicit).
    pub archived: Option<bool>,
}

impl Database {
    /// Insert a new document. Returns the inserted Document.
    ///
    /// `id` is taken from `meta.id` when supplied, else generated as
    /// UUIDv7. Conflict on id is a hard error.
    ///
    /// When the payload carries a `verification.criteria` array (#882
    /// step 1), any entry missing an `id` gets a fresh UUIDv7 assigned
    /// before the row is written -- the returned (and stored) payload
    /// carries the filled-in ids, so the caller can read them straight
    /// back off the id this call returns without a follow-up fetch. A
    /// payload with no such array (or none of whose entries need an id)
    /// is stored byte-identical to what was passed in.
    pub fn insert_document(&self, meta: &DocumentMeta<'_>, payload: &str) -> Result<Document> {
        let now = Utc::now().to_rfc3339();
        let id = match meta.id {
            Some(s) if !s.is_empty() => s.to_string(),
            _ => Uuid::now_v7().to_string(),
        };
        let status = meta.status.unwrap_or("draft");
        let payload = normalize_payload_criteria(&id, payload)?;

        self.conn.execute(
            "INSERT INTO documents (id, type, surface, status, priority, owner, payload, created_at, updated_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?8)",
            params![
                &id,
                meta.doc_type,
                meta.surface,
                status,
                meta.priority,
                meta.owner,
                &payload,
                &now,
            ],
        ).map_err(|e| match e {
            rusqlite::Error::SqliteFailure(ref err, _)
                if err.code == rusqlite::ErrorCode::ConstraintViolation =>
            {
                LegionError::WorkSource(format!("document id '{id}' already exists"))
            }
            other => LegionError::Database(other),
        })?;

        Ok(Document {
            id,
            doc_type: meta.doc_type.to_string(),
            surface: meta.surface.map(str::to_string),
            status: status.to_string(),
            priority: meta.priority.map(str::to_string),
            owner: meta.owner.to_string(),
            payload,
            archived_at: None,
            created_at: now.clone(),
            updated_at: now,
        })
    }

    /// Revise a document's payload in place, bumping the `revision` counter
    /// (#882 step 1). Errors when the document does not exist.
    ///
    /// Criterion ids in `verification.criteria` are preserved across the
    /// revision exactly when the incoming payload echoes them back: the
    /// expected flow is `document view` (read current ids), edit criterion
    /// text, resubmit each item with its `id` intact. An item submitted
    /// with no `id` (a genuinely new criterion) gets a fresh UUIDv7, same
    /// as `insert_document`. A duplicate `id` across two criteria in the
    /// same payload is a hard error (see `normalize_criteria`).
    ///
    /// Deliberately NOT built here (#882 step 1 design, scoped down):
    /// tracking which criterion ids a revision retires, or blocking a
    /// revision that would retire a criterion a live card is still
    /// servicing. The payload this call writes simply replaces what was
    /// there; a verdict citing an id that existed at a prior revision but
    /// not the current one is rejected by `verify::decide_spec` as an
    /// unknown criterion id, same as an id that never existed.
    ///
    /// Two guards on the whole-payload replace (#882 review, MED-5):
    /// refuses an already-archived document (archiving is meant to be
    /// terminal for editing, mirroring `archive_document`'s own filter),
    /// and -- when a live (non-cancelled) card is bound to this document --
    /// refuses a payload that drops the top-level `meta` object. The
    /// governed transition sync (`sync_bound_document`, `db::kanban`)
    /// hard-errors on a payload with no `meta` object to write `status`
    /// into, so a revise that dropped it would permanently wedge the bound
    /// card in a status it cannot leave through the governed path. This is
    /// the same invariant `archive_document` already enforces just above
    /// (orphaning a card from its spec is unacceptable) applied to
    /// structural corruption instead of archival.
    pub fn revise_document(&self, id: &str, payload: &str) -> Result<Document> {
        let existing = self
            .get_document(id)?
            .ok_or_else(|| LegionError::WorkSource(format!("document '{id}' not found")))?;
        if existing.archived_at.is_some() {
            return Err(LegionError::WorkSource(format!(
                "document '{id}' is archived and cannot be revised"
            )));
        }

        let now = Utc::now().to_rfc3339();
        let normalized_payload = normalize_payload_criteria(id, payload)?;

        if let Some(card_id) = self.live_card_bound_to_document(id)? {
            let has_meta = serde_json::from_str::<serde_json::Value>(&normalized_payload)
                .ok()
                .is_some_and(|v| v.get("meta").is_some_and(|m| m.is_object()));
            if !has_meta {
                return Err(LegionError::WorkSource(format!(
                    "document '{id}' is bound to live card '{card_id}': the revised payload \
                     must keep a top-level 'meta' object, or the card's governed status \
                     transitions would be permanently wedged"
                )));
            }
        }

        let rows = self.conn.execute(
            "UPDATE documents SET payload = ?1, revision = revision + 1, updated_at = ?2 \
             WHERE id = ?3 AND deleted_at IS NULL AND archived_at IS NULL",
            params![&normalized_payload, &now, id],
        )?;
        if rows == 0 {
            return Err(LegionError::WorkSource(format!(
                "document '{id}' not found"
            )));
        }
        self.get_document(id)?.ok_or_else(|| {
            LegionError::WorkSource(format!("document '{id}' vanished after revise"))
        })
    }

    /// Read a document's current `revision` counter (#882 step 1). A
    /// spec-bound verdict (`verify::SpecAcResult`) names the revision it
    /// was formed against; `cli::verify::handle_verify` reads this to
    /// confirm the citation is not stale before trusting a criterion id.
    pub fn document_revision(&self, id: &str) -> Result<i64> {
        self.conn
            .query_row(
                "SELECT revision FROM documents WHERE id = ?1 AND deleted_at IS NULL",
                params![id],
                |row| row.get(0),
            )
            .map_err(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => {
                    LegionError::WorkSource(format!("document '{id}' not found"))
                }
                other => LegionError::Database(other),
            })
    }

    /// Read a document by id. Returns None if the row is soft-deleted
    /// or does not exist. Does NOT filter archived rows -- caller can
    /// check `archived_at` on the returned Document.
    pub fn get_document(&self, id: &str) -> Result<Option<Document>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, type, surface, status, priority, owner, payload, archived_at, created_at, updated_at \
             FROM documents WHERE id = ?1 AND deleted_at IS NULL",
        )?;
        let mut rows = stmt.query(params![id])?;
        if let Some(row) = rows.next()? {
            Ok(Some(map_document_row(row)?))
        } else {
            Ok(None)
        }
    }

    /// List documents matching the filter. Returns rows ordered by
    /// updated_at DESC (most recently touched first).
    pub fn list_documents(&self, filter: &DocumentFilter<'_>) -> Result<Vec<Document>> {
        // Build the WHERE clause dynamically. Each filter clause adds a
        // bind parameter; archived clause is fixed SQL.
        let mut clauses: Vec<String> = vec!["deleted_at IS NULL".to_string()];
        let mut binds: Vec<String> = Vec::new();

        match filter.archived {
            None | Some(false) => clauses.push("archived_at IS NULL".to_string()),
            Some(true) => clauses.push("archived_at IS NOT NULL".to_string()),
        }

        if let Some(t) = filter.doc_type {
            clauses.push(format!("type = ?{}", binds.len() + 1));
            binds.push(t.to_string());
        }
        if let Some(s) = filter.surface {
            clauses.push(format!("surface = ?{}", binds.len() + 1));
            binds.push(s.to_string());
        }
        if let Some(st) = filter.status {
            clauses.push(format!("status = ?{}", binds.len() + 1));
            binds.push(st.to_string());
        }
        if let Some(o) = filter.owner {
            clauses.push(format!("owner = ?{}", binds.len() + 1));
            binds.push(o.to_string());
        }

        let sql = format!(
            "SELECT id, type, surface, status, priority, owner, payload, archived_at, created_at, updated_at \
             FROM documents WHERE {} ORDER BY updated_at DESC",
            clauses.join(" AND ")
        );

        let mut stmt = self.conn.prepare(&sql)?;
        let bind_refs: Vec<&dyn rusqlite::ToSql> =
            binds.iter().map(|s| s as &dyn rusqlite::ToSql).collect();
        let rows = stmt.query_map(bind_refs.as_slice(), map_document_row)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// Mark a document archived. Sets `archived_at = now`, updates
    /// `updated_at`. Idempotent: archiving an already-archived doc is
    /// a no-op success. Returns the updated Document.
    ///
    /// Errors when the document is bound to a live (non-cancelled, non-deleted)
    /// kanban card (#528): archiving a live requirement while work is in flight
    /// would orphan the card from its spec.
    pub fn archive_document(&self, id: &str) -> Result<Document> {
        // Guard: refuse archive when a live card is bound to this document.
        if let Some(card_id) = self.live_card_bound_to_document(id)? {
            return Err(LegionError::WorkSource(format!(
                "document '{id}' cannot be archived: live card '{card_id}' is bound to it"
            )));
        }

        let now = Utc::now().to_rfc3339();
        let rows = self.conn.execute(
            "UPDATE documents \
             SET archived_at = COALESCE(archived_at, ?1), updated_at = ?1 \
             WHERE id = ?2 AND deleted_at IS NULL",
            params![&now, id],
        )?;
        if rows == 0 {
            return Err(LegionError::WorkSource(format!(
                "document '{id}' not found"
            )));
        }
        self.get_document(id)?.ok_or_else(|| {
            LegionError::WorkSource(format!("document '{id}' vanished after archive"))
        })
    }

    /// Set a document's lifecycle status (e.g. `draft` -> `published`).
    /// Updates the `status` column and `updated_at`, returns the updated
    /// Document. This is the operator publish/approve action surfaced by
    /// the dashboard: the localhost session is the human gate, so there is
    /// no status-machine or adoption-gate enforcement here -- it is a
    /// direct flag set on the column the list/view read from.
    ///
    /// Note: the JSON payload may carry its own `meta.status` copy; this
    /// only writes the hoisted column, which is the source of truth for
    /// `list`/`view` filtering and the dashboard badge.
    pub fn set_document_status(&self, id: &str, status: &str) -> Result<Document> {
        let now = Utc::now().to_rfc3339();
        let rows = self.conn.execute(
            "UPDATE documents \
             SET status = ?1, updated_at = ?2 \
             WHERE id = ?3 AND deleted_at IS NULL",
            params![status, &now, id],
        )?;
        if rows == 0 {
            return Err(LegionError::WorkSource(format!(
                "document '{id}' not found"
            )));
        }
        self.get_document(id)?.ok_or_else(|| {
            LegionError::WorkSource(format!("document '{id}' vanished after set-status"))
        })
    }
}

/// Summary extracted from a validated schema payload, used to compose
/// the recall pointer reflection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SchemaSummary {
    pub title: String,
    pub description: String,
}

/// Validate a `doc_type = "schema"` payload as a structurally sound JSON
/// Schema (#526). Deliberately dependency-free: this is a shape gate, not
/// a full draft-07 validator. A payload passes when it is a JSON object
/// carrying `$schema` (string), `title` (string), `type: "object"`, and a
/// non-empty `properties` object; `required`, when present, must be an
/// array of strings naming keys that exist in `properties`.
pub fn validate_schema_payload(payload: &str) -> Result<SchemaSummary> {
    let value: serde_json::Value = serde_json::from_str(payload)
        .map_err(|e| LegionError::WorkSource(format!("schema payload is not valid JSON: {e}")))?;
    let obj = value
        .as_object()
        .ok_or_else(|| LegionError::WorkSource("schema payload must be a JSON object".into()))?;

    let str_field = |key: &str| -> Result<String> {
        obj.get(key)
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                LegionError::WorkSource(format!(
                    "schema payload missing required string field '{key}'"
                ))
            })
    };
    str_field("$schema")?;
    let title = str_field("title")?;
    let ty = str_field("type")?;
    if ty != "object" {
        return Err(LegionError::WorkSource(format!(
            "schema root 'type' must be \"object\", got \"{ty}\""
        )));
    }
    let properties = obj
        .get("properties")
        .and_then(|v| v.as_object())
        .filter(|m| !m.is_empty())
        .ok_or_else(|| {
            LegionError::WorkSource("schema payload needs a non-empty 'properties' object".into())
        })?;
    if let Some(required) = obj.get("required") {
        let names = required.as_array().ok_or_else(|| {
            LegionError::WorkSource("schema 'required' must be an array of strings".into())
        })?;
        for n in names {
            let name = n.as_str().ok_or_else(|| {
                LegionError::WorkSource("schema 'required' entries must be strings".into())
            })?;
            if !properties.contains_key(name) {
                return Err(LegionError::WorkSource(format!(
                    "schema 'required' names '{name}' which is not in 'properties'"
                )));
            }
        }
    }
    let description = obj
        .get("description")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    Ok(SchemaSummary { title, description })
}

/// Text for the schema pointer reflection (domain=schema) that makes a
/// landed schema document recallable (#526). The document row holds the
/// canonical payload; the reflection holds searchable prose plus the id.
pub fn schema_pointer_text(doc_id: &str, summary: &SchemaSummary) -> String {
    format!(
        "[SCHEMA] {} -- {} Canonical payload: legion document view {} (doc_type=schema).",
        summary.title,
        if summary.description.is_empty() {
            "no description.".to_string()
        } else {
            format!("{}.", summary.description.trim_end_matches('.'))
        },
        doc_id
    )
}

/// Validate an instance value against a subset of JSON Schema (#526):
/// `type`, `required`, `properties`, `items`, and `enum`. Returns one
/// human-readable error per violation, prefixed with a JSON-pointer-ish
/// path. An empty vec means the instance conforms to the checked subset.
///
/// Deliberately NOT a full validator (no $ref, no oneOf/allOf, no
/// format/pattern): the schemas legion lands are plain structural shapes,
/// and a dependency-free subset that rejects real mistakes beats a fake
/// pass-through. Unknown keywords are ignored, matching validator custom.
pub fn validate_instance(schema: &serde_json::Value, instance: &serde_json::Value) -> Vec<String> {
    let mut errors = Vec::new();
    check_value(schema, instance, "$", &mut errors);
    errors
}

fn type_name(v: &serde_json::Value) -> &'static str {
    match v {
        serde_json::Value::Null => "null",
        serde_json::Value::Bool(_) => "boolean",
        serde_json::Value::Number(n) => {
            if n.is_i64() || n.is_u64() {
                "integer"
            } else {
                "number"
            }
        }
        serde_json::Value::String(_) => "string",
        serde_json::Value::Array(_) => "array",
        serde_json::Value::Object(_) => "object",
    }
}

fn type_matches(expected: &str, v: &serde_json::Value) -> bool {
    match expected {
        // Every integer is a number; an integer-valued float is an integer.
        "number" => matches!(v, serde_json::Value::Number(_)),
        "integer" => match v {
            serde_json::Value::Number(n) => {
                n.is_i64() || n.is_u64() || n.as_f64().is_some_and(|f| f.fract() == 0.0)
            }
            _ => false,
        },
        other => other == type_name(v),
    }
}

fn check_value(
    schema: &serde_json::Value,
    instance: &serde_json::Value,
    path: &str,
    errors: &mut Vec<String>,
) {
    let Some(schema_obj) = schema.as_object() else {
        return; // non-object schema node: nothing in the subset to check
    };

    // `type` may be a single name or an array of alternatives
    // (e.g. ["string", "null"] for nullable fields).
    if let Some(ty) = schema_obj.get("type") {
        let alternatives: Vec<&str> = match ty {
            serde_json::Value::String(expected) => vec![expected.as_str()],
            serde_json::Value::Array(alts) => alts.iter().filter_map(|t| t.as_str()).collect(),
            _ => Vec::new(), // malformed type node: nothing in the subset to check
        };
        if !alternatives.is_empty() && !alternatives.iter().any(|e| type_matches(e, instance)) {
            errors.push(format!(
                "{path}: expected {}, got {}",
                alternatives.join(" or "),
                type_name(instance)
            ));
            return; // child checks would only cascade noise
        }
    }

    if let Some(allowed) = schema_obj.get("enum").and_then(|e| e.as_array())
        && !allowed.contains(instance)
    {
        errors.push(format!(
            "{path}: value {instance} not in enum {}",
            serde_json::Value::Array(allowed.clone())
        ));
    }

    if let Some(obj) = instance.as_object() {
        if let Some(required) = schema_obj.get("required").and_then(|r| r.as_array()) {
            for name in required.iter().filter_map(|n| n.as_str()) {
                if !obj.contains_key(name) {
                    errors.push(format!("{path}: missing required property '{name}'"));
                }
            }
        }
        if let Some(props) = schema_obj.get("properties").and_then(|p| p.as_object()) {
            for (name, child_schema) in props {
                if let Some(child) = obj.get(name) {
                    check_value(child_schema, child, &format!("{path}.{name}"), errors);
                }
            }
        }
    }

    if let Some(arr) = instance.as_array()
        && let Some(items) = schema_obj.get("items")
    {
        for (i, child) in arr.iter().enumerate() {
            check_value(items, child, &format!("{path}[{i}]"), errors);
        }
    }
}

/// Assign a UUIDv7 `id` to any `verification.criteria` entry that lacks one
/// (#882 step 1), in place. Returns `true` when at least one id was
/// assigned -- the caller uses this to decide whether the payload needs to
/// be re-serialized, so a payload with no `verification.criteria` array (or
/// one whose entries already all carry ids) is left byte-identical.
///
/// Hard-errors on a duplicate id across two criteria in the same array: a
/// shared id collapses `verify::decide_spec`'s required-id set to one
/// entry, silently letting a single verdict "cover" two criteria.
///
/// Also hard-errors (#882 simplify finding 2) on a `criteria` entry that is
/// not a JSON object: `resolve_spec_criteria` (src/cli/verify.rs, HIGH-2)
/// already refuses that same shape when reading criteria back for verify,
/// so leaving it untouched here would let a document be *written* with a
/// criterion that can *never* pass verify -- accepted at write time, then
/// hard-rejected downstream on someone else's card. Refusing here instead
/// puts the failure at the write the author controls. Because
/// `revise_document` always replaces the whole payload, this never
/// permanently strands an existing document: resubmitting a payload with
/// the offending entry fixed or dropped still succeeds.
fn normalize_criteria(doc_id: &str, payload_value: &mut serde_json::Value) -> Result<bool> {
    let Some(criteria) = payload_value
        .get_mut("verification")
        .and_then(|v| v.get_mut("criteria"))
        .and_then(|c| c.as_array_mut())
    else {
        return Ok(false);
    };

    let mut mutated = false;
    let mut seen_ids: std::collections::HashSet<String> = std::collections::HashSet::new();
    for (idx, item) in criteria.iter_mut().enumerate() {
        let Some(obj) = item.as_object_mut() else {
            return Err(LegionError::WorkSource(format!(
                "document '{doc_id}' verification.criteria[{idx}] is not a JSON object -- \
                 a malformed spec entry is refused, not silently written"
            )));
        };
        let existing_id = obj
            .get("id")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(str::to_string);
        let id = match existing_id {
            Some(id) => id,
            None => {
                let generated = Uuid::now_v7().to_string();
                obj.insert(
                    "id".to_string(),
                    serde_json::Value::String(generated.clone()),
                );
                mutated = true;
                generated
            }
        };
        if !seen_ids.insert(id.clone()) {
            return Err(LegionError::WorkSource(format!(
                "duplicate criterion id '{id}' in verification.criteria -- \
                 each criterion needs a unique id"
            )));
        }
    }
    Ok(mutated)
}

/// Parse `payload` as JSON and run [`normalize_criteria`] over it, returning
/// the (possibly rewritten) payload string. A payload that is not valid
/// JSON is returned unchanged -- this function is a best-effort normalizer,
/// not a second JSON validator (the CLI layer already validates payload
/// shape before calling `insert_document`/`revise_document`).
fn normalize_payload_criteria(doc_id: &str, payload: &str) -> Result<String> {
    let Ok(mut value) = serde_json::from_str::<serde_json::Value>(payload) else {
        return Ok(payload.to_string());
    };
    if normalize_criteria(doc_id, &mut value)? {
        Ok(value.to_string())
    } else {
        Ok(payload.to_string())
    }
}

fn map_document_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Document> {
    Ok(Document {
        id: row.get(0)?,
        doc_type: row.get(1)?,
        surface: row.get(2)?,
        status: row.get(3)?,
        priority: row.get(4)?,
        owner: row.get(5)?,
        payload: row.get(6)?,
        archived_at: row.get(7)?,
        created_at: row.get(8)?,
        updated_at: row.get(9)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::db::testutil::test_db;

    fn sample_meta<'a>(doc_type: &'a str, owner: &'a str) -> DocumentMeta<'a> {
        DocumentMeta {
            id: None,
            doc_type,
            surface: None,
            status: None,
            priority: None,
            owner,
        }
    }

    #[test]
    fn insert_and_get_document_round_trips() {
        let db = test_db();
        let meta = DocumentMeta {
            id: Some("FR-EMAIL-003"),
            doc_type: "requirement",
            surface: Some("email"),
            status: Some("specified"),
            priority: Some("SHALL"),
            owner: "mail",
        };
        let payload = r#"{"meta":{"id":"FR-EMAIL-003"},"title":"Thread detail"}"#;
        let inserted = db.insert_document(&meta, payload).expect("insert");
        assert_eq!(inserted.id, "FR-EMAIL-003");
        assert_eq!(inserted.doc_type, "requirement");
        assert_eq!(inserted.surface.as_deref(), Some("email"));
        assert_eq!(inserted.status, "specified");
        assert_eq!(inserted.priority.as_deref(), Some("SHALL"));
        assert_eq!(inserted.owner, "mail");
        assert_eq!(inserted.payload, payload);
        assert!(inserted.archived_at.is_none());

        let fetched = db.get_document("FR-EMAIL-003").expect("get").expect("some");
        assert_eq!(fetched.id, "FR-EMAIL-003");
        assert_eq!(fetched.payload, payload);
    }

    #[test]
    fn insert_without_id_generates_uuidv7() {
        let db = test_db();
        let inserted = db
            .insert_document(&sample_meta("persona", "vault"), "{}")
            .expect("insert");
        // UUIDv7 string is 36 chars (8-4-4-4-12 with dashes).
        assert_eq!(inserted.id.len(), 36);
        assert!(inserted.id.contains('-'));
    }

    #[test]
    fn insert_with_duplicate_id_errors() {
        let db = test_db();
        let mut meta = sample_meta("requirement", "mail");
        meta.id = Some("FR-TEST-001");
        db.insert_document(&meta, "{}").expect("first");
        let err = db.insert_document(&meta, "{}").unwrap_err();
        assert!(
            err.to_string().contains("already exists"),
            "expected conflict error, got: {err}"
        );
    }

    #[test]
    fn get_nonexistent_returns_none() {
        let db = test_db();
        assert!(db.get_document("FR-NOPE-999").expect("get").is_none());
    }

    #[test]
    fn list_documents_filters_by_type() {
        let db = test_db();
        db.insert_document(&sample_meta("requirement", "mail"), "{}")
            .unwrap();
        db.insert_document(&sample_meta("persona", "vault"), "{}")
            .unwrap();
        db.insert_document(&sample_meta("requirement", "platform"), "{}")
            .unwrap();

        let filter = DocumentFilter {
            doc_type: Some("requirement"),
            ..Default::default()
        };
        let rows = db.list_documents(&filter).expect("list");
        assert_eq!(rows.len(), 2);
        assert!(rows.iter().all(|r| r.doc_type == "requirement"));
    }

    #[test]
    fn list_documents_filters_by_surface_owner_status() {
        let db = test_db();
        let mut a = sample_meta("requirement", "mail");
        a.surface = Some("email");
        a.status = Some("specified");
        db.insert_document(&a, "{}").unwrap();

        let mut b = sample_meta("requirement", "platform");
        b.surface = Some("auth");
        b.status = Some("draft");
        db.insert_document(&b, "{}").unwrap();

        let filter = DocumentFilter {
            doc_type: Some("requirement"),
            surface: Some("email"),
            status: Some("specified"),
            owner: Some("mail"),
            ..Default::default()
        };
        let rows = db.list_documents(&filter).expect("list");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].surface.as_deref(), Some("email"));
    }

    #[test]
    fn list_documents_excludes_archived_by_default() {
        let db = test_db();
        let mut m = sample_meta("requirement", "mail");
        m.id = Some("FR-A");
        db.insert_document(&m, "{}").unwrap();
        m.id = Some("FR-B");
        db.insert_document(&m, "{}").unwrap();

        db.archive_document("FR-A").expect("archive");

        let hot = db.list_documents(&DocumentFilter::default()).expect("list");
        assert_eq!(hot.len(), 1);
        assert_eq!(hot[0].id, "FR-B");

        let cold = db
            .list_documents(&DocumentFilter {
                archived: Some(true),
                ..Default::default()
            })
            .expect("list");
        assert_eq!(cold.len(), 1);
        assert_eq!(cold[0].id, "FR-A");
    }

    #[test]
    fn archive_document_is_idempotent() {
        let db = test_db();
        let mut m = sample_meta("requirement", "mail");
        m.id = Some("FR-IDEM");
        db.insert_document(&m, "{}").unwrap();

        let first = db.archive_document("FR-IDEM").expect("first");
        assert!(first.archived_at.is_some());
        let first_ts = first.archived_at.clone();

        // Second archive does not change archived_at (COALESCE preserves
        // the original timestamp).
        let second = db.archive_document("FR-IDEM").expect("second");
        assert_eq!(second.archived_at, first_ts);
    }

    #[test]
    fn archive_nonexistent_returns_error() {
        let db = test_db();
        let err = db.archive_document("FR-NOPE").unwrap_err();
        assert!(err.to_string().contains("not found"));
    }

    #[test]
    fn set_document_status_flips_the_flag() {
        let db = test_db();
        let mut m = sample_meta("requirement", "mail");
        m.id = Some("FR-PUB");
        m.status = Some("draft");
        db.insert_document(&m, "{}").unwrap();

        let updated = db
            .set_document_status("FR-PUB", "published")
            .expect("set-status");
        assert_eq!(updated.status, "published");

        // Persisted, not just returned.
        let fetched = db.get_document("FR-PUB").expect("get").expect("some");
        assert_eq!(fetched.status, "published");
    }

    #[test]
    fn set_document_status_nonexistent_returns_error() {
        let db = test_db();
        let err = db.set_document_status("FR-NOPE", "published").unwrap_err();
        assert!(err.to_string().contains("not found"));
    }

    // -- archive guard: bound-live-card protection (#528) ------------------

    fn insert_bound_card(db: &Database, doc_id: &str) -> String {
        let card_id = db
            .insert_card(
                "legion",
                "legion",
                "bound card",
                None,
                crate::kanban::Priority::Med,
                None,
                None,
                None,
                None,
                None,
                crate::kanban::CardStatus::Accepted,
            )
            .expect("insert card");
        db.bind_card_to_document(&card_id, doc_id).expect("bind");
        card_id
    }

    /// archive_document errors when a live (non-cancelled) card is bound.
    #[test]
    fn archive_document_blocked_by_live_bound_card() {
        let db = test_db();
        let mut m = sample_meta("requirement", "mail");
        m.id = Some("FR-LIVE");
        db.insert_document(&m, "{}").unwrap();

        let card_id = insert_bound_card(&db, "FR-LIVE");

        let err = db.archive_document("FR-LIVE").unwrap_err();
        assert!(
            err.to_string().contains("cannot be archived"),
            "archive must be blocked: {err}"
        );
        assert!(
            err.to_string().contains(&card_id),
            "error must name the blocking card: {err}"
        );
    }

    /// archive_document succeeds when the bound card is cancelled.
    #[test]
    fn archive_document_allowed_when_bound_card_is_cancelled() {
        let db = test_db();
        let mut m = sample_meta("requirement", "mail");
        m.id = Some("FR-CANCEL");
        // force_move_card to "cancelled" now runs the same document-sync as
        // the governed move path (#753), so the payload needs a "meta"
        // object for the sync to have somewhere to write "status" into.
        db.insert_document(&m, r#"{"meta":{}}"#).unwrap();

        let card_id = insert_bound_card(&db, "FR-CANCEL");
        // Cancel the card.
        db.force_move_card(&card_id, "cancelled", None)
            .expect("cancel card");

        // Now archive_document should succeed.
        let doc = db.archive_document("FR-CANCEL").expect("archive");
        assert!(doc.archived_at.is_some(), "should be archived");
    }

    // -- schema payload validation (#526) ---------------------------------

    fn minimal_schema() -> String {
        serde_json::json!({
            "$schema": "http://json-schema.org/draft-07/schema#",
            "title": "Persona",
            "description": "A service-design persona",
            "type": "object",
            "properties": {
                "meta": {"type": "object"},
                "identity": {"type": "object"}
            },
            "required": ["meta", "identity"]
        })
        .to_string()
    }

    #[test]
    fn schema_payload_valid_returns_summary() {
        let s = validate_schema_payload(&minimal_schema()).expect("valid");
        assert_eq!(s.title, "Persona");
        assert_eq!(s.description, "A service-design persona");
    }

    #[test]
    fn schema_payload_rejects_non_json_and_non_object() {
        assert!(validate_schema_payload("not json").is_err());
        assert!(validate_schema_payload("[1,2]").is_err());
    }

    #[test]
    fn schema_payload_rejects_missing_fields() {
        for missing in ["$schema", "title", "type", "properties"] {
            let mut v: serde_json::Value = serde_json::from_str(&minimal_schema()).unwrap();
            v.as_object_mut().unwrap().remove(missing);
            let err = validate_schema_payload(&v.to_string()).unwrap_err();
            assert!(
                err.to_string().contains(missing) || missing == "properties",
                "expected error naming '{missing}', got: {err}"
            );
        }
    }

    #[test]
    fn schema_payload_rejects_required_naming_unknown_property() {
        let mut v: serde_json::Value = serde_json::from_str(&minimal_schema()).unwrap();
        v["required"] = serde_json::json!(["meta", "ghost"]);
        let err = validate_schema_payload(&v.to_string()).unwrap_err();
        assert!(err.to_string().contains("ghost"));
    }

    #[test]
    fn schema_payload_rejects_non_object_root_type() {
        let mut v: serde_json::Value = serde_json::from_str(&minimal_schema()).unwrap();
        v["type"] = serde_json::json!("array");
        assert!(validate_schema_payload(&v.to_string()).is_err());
    }

    #[test]
    fn schema_pointer_text_carries_title_and_id() {
        let s = SchemaSummary {
            title: "Persona".into(),
            description: "A service-design persona".into(),
        };
        let text = schema_pointer_text("doc-123", &s);
        assert!(text.starts_with("[SCHEMA] Persona"));
        assert!(text.contains("legion document view doc-123"));
    }

    // -- instance validation subset (#526) ---------------------------------

    #[test]
    fn instance_valid_passes() {
        let schema: serde_json::Value = serde_json::from_str(&minimal_schema()).unwrap();
        let inst = serde_json::json!({"meta": {}, "identity": {}});
        assert!(validate_instance(&schema, &inst).is_empty());
    }

    #[test]
    fn instance_missing_required_and_wrong_type_reported_with_paths() {
        let schema = serde_json::json!({
            "type": "object",
            "properties": {
                "meta": {"type": "object"},
                "needs": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "priority": {"type": "string", "enum": ["SHALL", "SHOULD", "MAY"]}
                        },
                        "required": ["priority"]
                    }
                }
            },
            "required": ["meta", "needs"]
        });
        let inst = serde_json::json!({
            "needs": [
                {"priority": "SHALL"},
                {"priority": "MUST"},
                {}
            ]
        });
        let errors = validate_instance(&schema, &inst);
        assert!(
            errors
                .iter()
                .any(|e| e.contains("missing required property 'meta'")),
            "{errors:?}"
        );
        assert!(
            errors
                .iter()
                .any(|e| e.contains("$.needs[1].priority") && e.contains("enum")),
            "{errors:?}"
        );
        assert!(
            errors
                .iter()
                .any(|e| e.contains("$.needs[2]") && e.contains("'priority'")),
            "{errors:?}"
        );
    }

    #[test]
    fn instance_type_mismatch_stops_child_cascade() {
        let schema = serde_json::json!({
            "type": "object",
            "properties": {"steps": {"type": "array", "items": {"type": "object"}}},
            "required": ["steps"]
        });
        let inst = serde_json::json!({"steps": "not-an-array"});
        let errors = validate_instance(&schema, &inst);
        assert_eq!(errors.len(), 1, "{errors:?}");
        assert!(errors[0].contains("$.steps") && errors[0].contains("expected array"));
    }

    #[test]
    fn instance_nullable_type_array() {
        let schema = serde_json::json!({
            "type": "object",
            "properties": {"persona": {"type": ["string", "null"]}}
        });
        assert!(validate_instance(&schema, &serde_json::json!({"persona": "maya"})).is_empty());
        assert!(validate_instance(&schema, &serde_json::json!({"persona": null})).is_empty());
        let errors = validate_instance(&schema, &serde_json::json!({"persona": 7}));
        assert_eq!(errors.len(), 1, "{errors:?}");
        assert!(errors[0].contains("$.persona"));
    }

    #[test]
    fn instance_integer_and_number_semantics() {
        let schema = serde_json::json!({
            "type": "object",
            "properties": {
                "score": {"type": "number"},
                "phase": {"type": "integer"}
            }
        });
        // integer satisfies number; float fails integer
        let ok = serde_json::json!({"score": 3, "phase": 2});
        assert!(validate_instance(&schema, &ok).is_empty());
        let bad = serde_json::json!({"score": 3.5, "phase": 2.5});
        let errors = validate_instance(&schema, &bad);
        assert_eq!(errors.len(), 1, "{errors:?}");
        assert!(errors[0].contains("$.phase"));
    }

    // -- criteria identity + revision (#882 step 1) -------------------------

    /// A fresh document starts at revision 1 -- the migration's DEFAULT and
    /// this insert path agree without a separate backfill.
    #[test]
    fn new_document_starts_at_revision_one() {
        let db = test_db();
        let doc = db
            .insert_document(&sample_meta("requirement", "mail"), "{}")
            .expect("insert");
        assert_eq!(db.document_revision(&doc.id).expect("revision"), 1);
    }

    #[test]
    fn document_revision_of_unknown_id_errors() {
        let db = test_db();
        assert!(db.document_revision("nope").is_err());
    }

    /// A criterion with no `id` gets one assigned at insert; the returned
    /// (and stored) payload carries it.
    #[test]
    fn insert_document_assigns_ids_to_criteria_missing_one() {
        let db = test_db();
        let payload = serde_json::json!({
            "verification": {
                "criteria": [
                    {"text": "does the thing"},
                    {"text": "does the other thing"}
                ]
            }
        })
        .to_string();
        let doc = db
            .insert_document(&sample_meta("requirement", "mail"), &payload)
            .expect("insert");
        let value: serde_json::Value = serde_json::from_str(&doc.payload).unwrap();
        let criteria = value["verification"]["criteria"].as_array().unwrap();
        assert_eq!(criteria.len(), 2);
        for c in criteria {
            let id = c["id"].as_str().expect("id assigned");
            assert!(
                uuid::Uuid::parse_str(id).is_ok_and(|u| u.get_version_num() == 7),
                "expected a UUIDv7, got {id}"
            );
        }
        // Ids must be distinct.
        assert_ne!(criteria[0]["id"], criteria[1]["id"]);
    }

    /// A caller-supplied id is kept verbatim, not overwritten.
    #[test]
    fn insert_document_preserves_caller_supplied_criterion_id() {
        let db = test_db();
        let payload = serde_json::json!({
            "verification": {"criteria": [{"id": "crit-fixed", "text": "does the thing"}]}
        })
        .to_string();
        let doc = db
            .insert_document(&sample_meta("requirement", "mail"), &payload)
            .expect("insert");
        let value: serde_json::Value = serde_json::from_str(&doc.payload).unwrap();
        assert_eq!(value["verification"]["criteria"][0]["id"], "crit-fixed");
    }

    /// A payload with no `verification.criteria` array is stored
    /// byte-identical -- normalization must not touch unrelated payloads.
    #[test]
    fn insert_document_without_criteria_is_untouched() {
        let db = test_db();
        let payload = r#"{"meta":{"id":"FR-X"},"title":"Thread detail"}"#;
        let doc = db
            .insert_document(&sample_meta("requirement", "mail"), payload)
            .expect("insert");
        assert_eq!(doc.payload, payload);
    }

    /// Two criteria sharing the same explicit id is a hard error at insert.
    #[test]
    fn insert_document_rejects_duplicate_criterion_ids() {
        let db = test_db();
        let payload = serde_json::json!({
            "verification": {
                "criteria": [
                    {"id": "dup", "text": "first"},
                    {"id": "dup", "text": "second"}
                ]
            }
        })
        .to_string();
        let err = db
            .insert_document(&sample_meta("requirement", "mail"), &payload)
            .unwrap_err();
        assert!(
            err.to_string().contains("duplicate criterion id"),
            "got: {err}"
        );
    }

    /// revise_document bumps the revision and, when the caller echoes an
    /// existing criterion's id back, preserves it -- while a new criterion
    /// with no id gets a fresh one.
    #[test]
    fn revise_document_bumps_revision_and_preserves_echoed_ids() {
        let db = test_db();
        let initial = serde_json::json!({
            "verification": {"criteria": [{"text": "first criterion"}]}
        })
        .to_string();
        let doc = db
            .insert_document(&sample_meta("requirement", "mail"), &initial)
            .expect("insert");
        let first_id = serde_json::from_str::<serde_json::Value>(&doc.payload).unwrap()
            ["verification"]["criteria"][0]["id"]
            .as_str()
            .unwrap()
            .to_string();
        assert_eq!(db.document_revision(&doc.id).unwrap(), 1);

        // Revise: keep the first criterion (echoing its id, edited text) and
        // add a second, brand-new criterion with no id.
        let revised_payload = serde_json::json!({
            "verification": {
                "criteria": [
                    {"id": first_id, "text": "first criterion, reworded"},
                    {"text": "second criterion"}
                ]
            }
        })
        .to_string();
        let revised = db
            .revise_document(&doc.id, &revised_payload)
            .expect("revise");
        assert_eq!(db.document_revision(&doc.id).unwrap(), 2);

        let value: serde_json::Value = serde_json::from_str(&revised.payload).unwrap();
        let criteria = value["verification"]["criteria"].as_array().unwrap();
        assert_eq!(criteria.len(), 2);
        assert_eq!(criteria[0]["id"], first_id, "echoed id must be preserved");
        assert_eq!(criteria[0]["text"], "first criterion, reworded");
        let second_id = criteria[1]["id"].as_str().expect("second id assigned");
        assert_ne!(second_id, first_id, "the new criterion gets its own id");
    }

    #[test]
    fn revise_document_nonexistent_returns_error() {
        let db = test_db();
        let err = db.revise_document("nope", "{}").unwrap_err();
        assert!(err.to_string().contains("not found"));
    }

    // -- revise guard: bound-live-card protection + archived filter (#882 review, MED-5) --

    /// A document bound to a live card must keep its `meta` object across a
    /// revise -- the governed transition sync (`sync_bound_document`,
    /// src/db/kanban.rs) hard-errors on a payload with no `meta` object, so
    /// dropping it would wedge the card in a status it cannot leave through
    /// the governed path. Mirrors `archive_document_blocked_by_live_bound_card`.
    #[test]
    fn revise_document_blocked_when_bound_card_would_be_wedged() {
        let db = test_db();
        let mut m = sample_meta("requirement", "mail");
        m.id = Some("FR-WEDGE");
        db.insert_document(&m, r#"{"meta":{}}"#).unwrap();

        let card_id = insert_bound_card(&db, "FR-WEDGE");

        // The revise payload drops 'meta' entirely.
        let err = db.revise_document("FR-WEDGE", "{}").unwrap_err();
        assert!(
            err.to_string().contains("meta"),
            "error must name the missing 'meta' object: {err}"
        );
        assert!(
            err.to_string().contains(&card_id) || err.to_string().contains("live"),
            "error must explain the live-card wedge risk: {err}"
        );

        // Refused: the original payload (with meta) must still be in place.
        let fetched = db.get_document("FR-WEDGE").unwrap().unwrap();
        assert!(fetched.payload.contains("meta"));
    }

    /// The same document, same drop-meta payload, succeeds once the bound
    /// card is cancelled (no longer live) -- mirrors
    /// `archive_document_allowed_when_bound_card_is_cancelled`.
    #[test]
    fn revise_document_allowed_when_bound_card_is_cancelled() {
        let db = test_db();
        let mut m = sample_meta("requirement", "mail");
        m.id = Some("FR-WEDGE-CANCEL");
        db.insert_document(&m, r#"{"meta":{}}"#).unwrap();

        let card_id = insert_bound_card(&db, "FR-WEDGE-CANCEL");
        db.force_move_card(&card_id, "cancelled", None)
            .expect("cancel card");

        let revised = db.revise_document("FR-WEDGE-CANCEL", "{}").expect("revise");
        assert_eq!(revised.payload, "{}");
    }

    /// A document bound to a live card may still be revised as long as the
    /// new payload keeps a `meta` object -- the guard only refuses payloads
    /// that would actually wedge the card, not every revise of a bound doc.
    #[test]
    fn revise_document_allowed_when_bound_card_and_meta_preserved() {
        let db = test_db();
        let mut m = sample_meta("requirement", "mail");
        m.id = Some("FR-WEDGE-OK");
        db.insert_document(&m, r#"{"meta":{}}"#).unwrap();
        insert_bound_card(&db, "FR-WEDGE-OK");

        let revised = db
            .revise_document("FR-WEDGE-OK", r#"{"meta":{"status":"draft"}}"#)
            .expect("revise with meta preserved must succeed");
        assert!(revised.payload.contains("meta"));
    }

    /// An archived document cannot be revised (the revise UPDATE now also
    /// filters `archived_at IS NULL`, not just `deleted_at IS NULL`).
    #[test]
    fn revise_document_refuses_archived_document() {
        let db = test_db();
        let mut m = sample_meta("requirement", "mail");
        m.id = Some("FR-ARCHIVED");
        db.insert_document(&m, "{}").unwrap();
        db.archive_document("FR-ARCHIVED").expect("archive");

        let err = db.revise_document("FR-ARCHIVED", "{}").unwrap_err();
        assert!(
            err.to_string().contains("archived"),
            "error must say the document is archived: {err}"
        );
    }

    // -- normalize_criteria: non-object entries (#882 simplify finding 2) --

    /// A `criteria` entry that is not a JSON object (e.g. a bare string)
    /// must be refused at insert, not written through untouched --
    /// `resolve_spec_criteria` (src/cli/verify.rs, HIGH-2) already refuses
    /// that same shape when reading criteria back for verify, so accepting
    /// it here would create a document that can never pass verify.
    #[test]
    fn insert_document_rejects_non_object_criterion_entry() {
        let db = test_db();
        let payload = serde_json::json!({
            "verification": {
                "criteria": [
                    {"text": "does the thing"},
                    "not an object"
                ]
            }
        })
        .to_string();
        let err = db
            .insert_document(&sample_meta("requirement", "mail"), &payload)
            .unwrap_err();
        assert!(
            err.to_string().contains("criteria[1]"),
            "error must name the offending index: {err}"
        );
    }

    /// A document with a malformed criteria entry already in storage
    /// (representing data written before this guard existed) is not
    /// permanently stuck: `revise_document` replaces the whole payload, so
    /// resubmitting a corrected criteria array -- with the bad entry
    /// dropped -- still succeeds. A write-time refusal must not make an
    /// existing document unrevisable.
    #[test]
    fn revise_document_recovers_a_document_with_a_preexisting_malformed_entry() {
        let db = test_db();
        let mut m = sample_meta("requirement", "mail");
        m.id = Some("FR-LEGACY-BAD");
        db.insert_document(&m, r#"{"meta":{}}"#).unwrap();

        // Simulate data already in storage before this guard existed:
        // write a non-object criteria entry directly, bypassing
        // normalize_criteria.
        db.conn
            .execute(
                "UPDATE documents SET payload = ?1 WHERE id = ?2",
                params![
                    r#"{"meta":{},"verification":{"criteria":["not an object"]}}"#,
                    "FR-LEGACY-BAD",
                ],
            )
            .unwrap();

        let fixed = serde_json::json!({
            "meta": {},
            "verification": {"criteria": [{"text": "fixed criterion"}]}
        })
        .to_string();
        let revised = db.revise_document("FR-LEGACY-BAD", &fixed).expect(
            "revise with a corrected payload must succeed even though the stored \
                     document previously carried a malformed entry",
        );
        assert!(revised.payload.contains("fixed criterion"));
    }
}
