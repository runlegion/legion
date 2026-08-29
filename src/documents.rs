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

use crate::db::{Database, ReflectionMeta};
use crate::error::{LegionError, Result};
use crate::search::SearchIndex;

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
    /// Guard on the whole-payload replace (#882 review, MED-5): refuses an
    /// already-archived document (archiving is meant to be terminal for
    /// editing, mirroring `archive_document`'s own filter).
    ///
    /// A second guard used to run here -- when a live (non-cancelled) card
    /// was bound to this document, a payload dropping the top-level `meta`
    /// object was refused, because the governed transition sync
    /// (`sync_bound_document`, formerly `db::kanban`) hard-errored on a
    /// payload with no `meta` object to write `status` into. #931 removed
    /// the card surface, including card<->document binding
    /// (`bind_card_to_document`, `tasks.document_id`) and its governed sync
    /// -- there is no card left that could be wedged by a dropped `meta`
    /// object, so the guard has nothing left to protect and is gone with it.
    pub fn revise_document(&self, id: &str, payload: &str) -> Result<Document> {
        let existing = self
            .get_document(id)?
            .ok_or_else(|| LegionError::WorkSource(format!("document '{id}' not found")))?;
        if existing.archived_at.is_some() {
            return Err(LegionError::WorkSource(format!(
                "document '{id}' is archived and cannot be edited"
            )));
        }

        let now = Utc::now().to_rfc3339();
        let normalized_payload = normalize_payload_criteria(id, payload)?;

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

    /// Merge `body` into the document's payload under the top-level `body`
    /// key (creating the key if absent) and write it back WITHOUT touching
    /// `revision`. Only `updated_at` and `payload` change.
    ///
    /// Backs the editor's debounced working-copy save (#1036): the caller
    /// can call this on every keystroke pause without cutting a new
    /// revision -- a revision is cut only by an explicit `revise_document`
    /// call or a status change. Errors when the document does not exist or
    /// is archived, mirroring `revise_document`'s own guards.
    pub fn update_document_body(&self, id: &str, body: &str) -> Result<Document> {
        let existing = self
            .get_document(id)?
            .ok_or_else(|| LegionError::WorkSource(format!("document '{id}' not found")))?;
        if existing.archived_at.is_some() {
            return Err(LegionError::WorkSource(format!(
                "document '{id}' is archived and cannot be edited"
            )));
        }

        // Pre-check the stored payload is a JSON object so the `json_set`
        // write below never raises -- it would otherwise silently no-op on
        // a non-object root, which would look like a successful save that
        // actually dropped the caller's body text.
        let value: serde_json::Value = serde_json::from_str(&existing.payload).map_err(|e| {
            LegionError::WorkSource(format!("document '{id}' payload is not valid JSON: {e}"))
        })?;
        if !value.is_object() {
            return Err(LegionError::WorkSource(format!(
                "document '{id}' payload is not a JSON object"
            )));
        }

        let now = Utc::now().to_rfc3339();
        // A single UPDATE ... json_set(payload, '$.body', ?1) (#1036 review,
        // MED-1) is what closes the read-modify-write window: the previous
        // implementation fetched `payload`, merged `body` in on the Rust
        // side, and wrote the merged copy back, so a `revise_document` that
        // committed between that fetch and this write would have been
        // silently overwritten by the stale pre-revise snapshot. json_set is
        // evaluated by SQLite against the row's CURRENT value at UPDATE
        // time, not a value fetched earlier by this call, so that window no
        // longer exists. The actual interleaving is not unit-tested here --
        // that would need two real concurrent connections racing on one
        // row -- `update_document_body_after_revise_keeps_revised_fields_and_leaves_revision_unchanged`
        // below only proves the sequential contract: a body save issued
        // after a revise has already committed still keeps the revise's
        // fields and still does not bump revision. This also preserves the
        // payload's existing key order -- the old Value::Object round trip
        // did not, since serde_json does not enable the preserve_order
        // feature here.
        let rows = self.conn.execute(
            "UPDATE documents SET payload = json_set(payload, '$.body', ?1), updated_at = ?2 \
             WHERE id = ?3 AND deleted_at IS NULL AND archived_at IS NULL",
            params![body, &now, id],
        )?;
        if rows == 0 {
            return Err(LegionError::WorkSource(format!(
                "document '{id}' not found"
            )));
        }
        self.get_document(id)?.ok_or_else(|| {
            LegionError::WorkSource(format!("document '{id}' vanished after body update"))
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
    /// Used to refuse archiving a document bound to a live kanban card
    /// (#528): archiving a live requirement while work is in flight would
    /// orphan the card from its spec. #931 removed card<->document binding
    /// along with the rest of the card surface, so there is no longer a
    /// card that could be orphaned this way, and the guard is gone with it.
    pub fn archive_document(&self, id: &str) -> Result<Document> {
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

    /// Retrieve every document for reindexing (#1037), archived included.
    ///
    /// Mirrors `get_all_for_reindex` for reflections: only soft-deleted
    /// rows (`deleted_at`) are excluded. `list_documents`'s `archived`
    /// filter can only select hot-only or archived-only, never both --
    /// using it here would silently drop archived documents from a
    /// rebuilt index even though they must stay searchable (the write path
    /// re-indexes an archived document on `archive_document_indexed`, so
    /// this method and that write path must agree on what "every document"
    /// means).
    pub fn get_all_documents_for_reindex(&self) -> Result<Vec<Document>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, type, surface, status, priority, owner, payload, archived_at, created_at, updated_at \
             FROM documents WHERE deleted_at IS NULL",
        )?;
        let rows = stmt.query_map([], map_document_row)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }
}

/// Extract the searchable text for a document's search-index entry
/// (#1037): `title`, `description`, and `body` (all three, when present --
/// the app's top-bar search is over documents whose content IS
/// `payload.body`, the editor's document, so it cannot be dropped in
/// favor of `description`) from the parsed payload, plus `doc_type` and
/// `surface` -- the same fields a human would scan when picking a document
/// out of a list.
///
/// Infallible: a payload that is not valid JSON, or not a JSON object,
/// contributes no text beyond `doc_type`/`surface` rather than failing --
/// this runs after the document's DB write has already committed, so a
/// malformed payload must never turn a successful write into a failed
/// index update.
pub fn document_search_text(doc: &Document) -> String {
    let mut parts: Vec<String> = Vec::new();

    if let Ok(serde_json::Value::Object(obj)) = serde_json::from_str(&doc.payload) {
        for field in ["title", "description", "body"] {
            if let Some(value) = obj.get(field).and_then(|v| v.as_str()) {
                parts.push(value.to_string());
            }
        }
    }

    parts.push(doc.doc_type.clone());
    if let Some(surface) = &doc.surface {
        parts.push(surface.clone());
    }

    parts.join(" ")
}

/// Write a document's search-index entry (#1037): derive its searchable
/// text via [`document_search_text`] and hand it to
/// `SearchIndex::add_document`, which deletes any prior entry for the same
/// id before adding the new one. Shared by every `*_indexed` write-path
/// wrapper below so the derive-then-add step exists in exactly one place.
fn index_document(index: &SearchIndex, doc: &Document) -> Result<()> {
    index.add_document(
        &doc.id,
        &doc.owner,
        &document_search_text(doc),
        &doc.created_at,
    )
}

/// Insert a new document and index it for search in one call (#1037).
///
/// Composes `Database::insert_document` with `SearchIndex::add_document`
/// the same way `reflect_from_text_with_meta` composes reflection insert
/// with reflection indexing: the DB write is the source of truth, the
/// index write follows it. `repo` in the index is the document's `owner`
/// column (`SearchIndex::add_document`'s doc comment).
pub fn insert_document_indexed(
    db: &Database,
    index: &SearchIndex,
    meta: &DocumentMeta<'_>,
    payload: &str,
) -> Result<Document> {
    let doc = db.insert_document(meta, payload)?;
    index_document(index, &doc)?;
    Ok(doc)
}

/// Revise a document's payload and re-index it for search in one call
/// (#1037). `SearchIndex::add_document` deletes the prior entry for this
/// id before adding the new one, so a revise never leaves a stale-text
/// ghost or a duplicate hit behind.
pub fn revise_document_indexed(
    db: &Database,
    index: &SearchIndex,
    id: &str,
    payload: &str,
) -> Result<Document> {
    let doc = db.revise_document(id, payload)?;
    index_document(index, &doc)?;
    Ok(doc)
}

/// Save the editor's working-copy body and re-index it for search in one
/// call (#1037). `body` is part of `document_search_text`'s indexed text,
/// so a body save that skipped re-indexing would silently diverge the
/// index from the row on every edit -- and this is the highest-frequency
/// document write path, called on every debounced editor keystroke pause.
pub fn update_document_body_indexed(
    db: &Database,
    index: &SearchIndex,
    id: &str,
    body: &str,
) -> Result<Document> {
    let doc = db.update_document_body(id, body)?;
    index_document(index, &doc)?;
    Ok(doc)
}

/// Set a document's lifecycle status and re-index it for search in one
/// call (#1037). The status change does not alter the searchable text,
/// but re-indexing keeps the index's `created_at`/`repo` fields aligned
/// with the row and (via the delete-first semantics of `add_document`)
/// guards against ever accumulating more than one index entry per id.
pub fn set_document_status_indexed(
    db: &Database,
    index: &SearchIndex,
    id: &str,
    status: &str,
) -> Result<Document> {
    let doc = db.set_document_status(id, status)?;
    index_document(index, &doc)?;
    Ok(doc)
}

/// Archive a document and re-index it for search in one call (#1037).
///
/// Archived documents stay searchable (`get_document`/`GET
/// /api/documents/{id}` still serve them; only `list_documents`'s default
/// filter hides them), so archiving re-indexes rather than deletes.
pub fn archive_document_indexed(db: &Database, index: &SearchIndex, id: &str) -> Result<Document> {
    let doc = db.archive_document(id)?;
    index_document(index, &doc)?;
    Ok(doc)
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

/// Max characters kept from a schema's `title`/`description` when composing
/// the pointer reflection text below. `title`/`description` come from the
/// schema payload's own content, which -- via the HTTP create/revise
/// endpoints (#1036) -- is now network input rather than only a local
/// operator's own file, so a cap keeps a hostile payload from flooding the
/// shared bullpen/recall feed with an oversized entry.
const MAX_POINTER_FIELD_LEN: usize = 300;

/// Collapse newlines to spaces and cap length so a schema's `title`/
/// `description` cannot forge extra lines into the pointer reflection's
/// text or blow past a reasonable recall-feed entry size (#1036 review,
/// MED-4).
fn sanitize_pointer_field(s: &str) -> String {
    s.chars()
        .map(|c| if c == '\n' || c == '\r' { ' ' } else { c })
        .take(MAX_POINTER_FIELD_LEN)
        .collect()
}

/// Text for the schema pointer reflection (domain=schema) that makes a
/// landed schema document recallable (#526). The document row holds the
/// canonical payload; the reflection holds searchable prose plus the id.
pub fn schema_pointer_text(doc_id: &str, summary: &SchemaSummary) -> String {
    let title = sanitize_pointer_field(&summary.title);
    let description = sanitize_pointer_field(&summary.description);
    format!(
        "[SCHEMA] {} -- {} Canonical payload: legion document view {} (doc_type=schema).",
        title,
        if description.is_empty() {
            "no description.".to_string()
        } else {
            format!("{}.", description.trim_end_matches('.'))
        },
        doc_id
    )
}

/// Write (or refresh) the domain=schema pointer reflection for a landed
/// schema document (#526): the document row holds the canonical payload,
/// this reflection holds the searchable prose + id so `legion recall
/// --domain schema` can find it. Shared (#1036 review, MED-3) by the CLI's
/// create/revise arms and the HTTP create/revise handlers so the dual-write
/// shape cannot fork between entry points -- each caller wraps the raw
/// error in its own remediation message, so this returns it unwrapped.
pub fn write_schema_pointer(db: &Database, doc: &Document, summary: &SchemaSummary) -> Result<()> {
    let text = schema_pointer_text(&doc.id, summary);
    let meta = ReflectionMeta {
        domain: Some("schema".to_string()),
        tags: Some("schema,document-pointer".to_string()),
        parent_id: None,
    };
    db.insert_reflection_with_meta(&doc.owner, &text, "self", &meta)?;
    Ok(())
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

    // The archive/revise guards this section used to test (refusing to
    // archive or revise a document with a live card bound) were removed
    // along with card<->document binding itself (#931) -- see
    // `archive_document`'s and `revise_document`'s doc comments.

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

    /// A newline in a schema's title/description cannot forge extra lines
    /// into the pointer text, and an oversized field is truncated rather
    /// than flooding the recall feed (#1036 review, MED-4).
    #[test]
    fn schema_pointer_text_sanitizes_title_and_description() {
        let s = SchemaSummary {
            title: "Persona\nline two".into(),
            description: "x".repeat(MAX_POINTER_FIELD_LEN + 50),
        };
        let text = schema_pointer_text("doc-123", &s);
        assert!(!text.contains('\n'), "newline must not survive: {text}");
        assert!(text.contains("Persona line two"));
        assert!(
            text.len() < MAX_POINTER_FIELD_LEN + 200,
            "an oversized description must be truncated, got {} chars",
            text.len()
        );
    }

    /// `write_schema_pointer` inserts a domain=schema reflection naming the
    /// document, the shared shape the CLI create/revise arms and the HTTP
    /// create/revise handlers all call (#1036 review, MED-3).
    #[test]
    fn write_schema_pointer_inserts_domain_schema_reflection() {
        let db = test_db();
        let mut m = sample_meta("schema", "vault");
        m.id = Some("SCHEMA-1");
        let doc = db.insert_document(&m, "{}").expect("insert");
        let summary = SchemaSummary {
            title: "Widget".to_string(),
            description: "A test schema".to_string(),
        };
        write_schema_pointer(&db, &doc, &summary).expect("write pointer");

        let reflections = db
            .get_reflections_by_domain(
                "vault",
                "schema",
                10,
                crate::recall::ArchiveMode::Hot,
                &crate::timerange::TimeRange::default(),
            )
            .expect("get reflections by domain");
        assert!(
            reflections
                .iter()
                .any(|r| r.text.contains("Widget") && r.text.contains("SCHEMA-1")),
            "expected the pointer reflection naming the document, got: {reflections:?}"
        );
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

    // The revise guard this section used to test (refusing to drop 'meta'
    // from a document bound to a live card) was removed along with
    // card<->document binding itself (#931) -- see `revise_document`'s doc
    // comment.

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

    // -- update_document_body (#1036) --

    /// `update_document_body` sets the top-level `body` key (creating it
    /// when absent, overwriting it when present) while leaving every other
    /// payload field and the `revision` counter untouched -- the editor's
    /// debounced save must never cut a revision.
    #[test]
    fn update_document_body_merges_key_and_leaves_revision_unchanged() {
        let db = test_db();
        let doc = db
            .insert_document(&sample_meta("thesis", "mail"), r#"{"title":"x"}"#)
            .expect("insert");
        assert_eq!(db.document_revision(&doc.id).unwrap(), 1);

        let updated = db
            .update_document_body(&doc.id, "first draft")
            .expect("update body");
        let value: serde_json::Value = serde_json::from_str(&updated.payload).unwrap();
        assert_eq!(value["title"], "x", "unrelated fields survive the merge");
        assert_eq!(value["body"], "first draft");
        assert_eq!(
            db.document_revision(&doc.id).unwrap(),
            1,
            "a body save must not bump revision"
        );

        // A second save overwrites rather than appending.
        let updated = db
            .update_document_body(&doc.id, "second draft")
            .expect("update body again");
        let value: serde_json::Value = serde_json::from_str(&updated.payload).unwrap();
        assert_eq!(value["body"], "second draft");
        assert_eq!(db.document_revision(&doc.id).unwrap(), 1);
    }

    #[test]
    fn update_document_body_nonexistent_returns_error() {
        let db = test_db();
        let err = db.update_document_body("nope", "text").unwrap_err();
        assert!(err.to_string().contains("not found"));
    }

    /// An archived document cannot have its working copy saved, mirroring
    /// `revise_document_refuses_archived_document`.
    #[test]
    fn update_document_body_refuses_archived_document() {
        let db = test_db();
        let mut m = sample_meta("thesis", "mail");
        m.id = Some("TH-ARCHIVED");
        db.insert_document(&m, "{}").unwrap();
        db.archive_document("TH-ARCHIVED").expect("archive");

        let err = db.update_document_body("TH-ARCHIVED", "text").unwrap_err();
        assert!(
            err.to_string().contains("archived"),
            "error must say the document is archived: {err}"
        );
    }

    /// A body save issued after a revise has already committed builds on
    /// the revised payload, not a stale pre-revise snapshot, and still does
    /// not bump revision (#1036 review). This is a sequential check with a
    /// single connection -- it does not exercise the read-modify-write
    /// interleaving `update_document_body`'s `json_set` UPDATE closes (see
    /// that function's doc comment); proving the interleaving itself would
    /// need two real concurrent connections racing on one row, which is not
    /// set up here.
    #[test]
    fn update_document_body_after_revise_keeps_revised_fields_and_leaves_revision_unchanged() {
        let db = test_db();
        let mut m = sample_meta("thesis", "mail");
        m.id = Some("TH-AFTER-REVISE-1");
        db.insert_document(&m, r#"{"title":"a"}"#).unwrap();

        db.update_document_body("TH-AFTER-REVISE-1", "draft one")
            .unwrap();

        let revised_payload = serde_json::json!({"title": "b", "extra": "x"}).to_string();
        db.revise_document("TH-AFTER-REVISE-1", &revised_payload)
            .unwrap();
        assert_eq!(db.document_revision("TH-AFTER-REVISE-1").unwrap(), 2);

        let updated = db
            .update_document_body("TH-AFTER-REVISE-1", "draft two")
            .expect("update body after revise");
        let value: serde_json::Value = serde_json::from_str(&updated.payload).unwrap();
        assert_eq!(
            value["title"], "b",
            "revise's fields must survive a later body save"
        );
        assert_eq!(value["extra"], "x");
        assert_eq!(value["body"], "draft two");
        assert_eq!(
            db.document_revision("TH-AFTER-REVISE-1").unwrap(),
            2,
            "a body save must not bump revision even after an intervening revise"
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

    // -- #1037: documents in the search index ------------------------------

    #[test]
    fn document_search_text_uses_title_and_description() {
        let doc = Document {
            id: "doc-1".into(),
            doc_type: "requirement".into(),
            surface: Some("email".into()),
            status: "draft".into(),
            priority: None,
            owner: "mail".into(),
            payload: r#"{"title":"Thread detail","description":"how threads render"}"#.into(),
            archived_at: None,
            created_at: "2026-01-01T00:00:00+00:00".into(),
            updated_at: "2026-01-01T00:00:00+00:00".into(),
        };
        let text = document_search_text(&doc);
        assert!(text.contains("Thread detail"));
        assert!(text.contains("how threads render"));
        assert!(text.contains("requirement"));
        assert!(text.contains("email"));
    }

    #[test]
    fn document_search_text_includes_body_without_description() {
        let doc = Document {
            id: "doc-1".into(),
            doc_type: "nfr".into(),
            surface: None,
            status: "draft".into(),
            priority: None,
            owner: "mail".into(),
            payload: r#"{"title":"Latency budget","body":"p99 under 200ms"}"#.into(),
            archived_at: None,
            created_at: "2026-01-01T00:00:00+00:00".into(),
            updated_at: "2026-01-01T00:00:00+00:00".into(),
        };
        let text = document_search_text(&doc);
        assert!(text.contains("Latency budget"));
        assert!(text.contains("p99 under 200ms"));
    }

    /// The app's top-bar search is over documents whose content IS
    /// `payload.body` (the editor's document), so `description` must not
    /// suppress `body` -- both are indexed together, alongside `title`.
    #[test]
    fn document_search_text_includes_title_description_and_body_together() {
        let doc = Document {
            id: "doc-1".into(),
            doc_type: "spec".into(),
            surface: None,
            status: "draft".into(),
            priority: None,
            owner: "mail".into(),
            payload: r#"{"title":"Thread detail","description":"how threads render","body":"the full editor document text"}"#.into(),
            archived_at: None,
            created_at: "2026-01-01T00:00:00+00:00".into(),
            updated_at: "2026-01-01T00:00:00+00:00".into(),
        };
        let text = document_search_text(&doc);
        assert!(text.contains("Thread detail"));
        assert!(text.contains("how threads render"));
        assert!(
            text.contains("the full editor document text"),
            "body must not be dropped when description is present: {text}"
        );
    }

    #[test]
    fn document_search_text_handles_malformed_payload_without_failing() {
        let doc = Document {
            id: "doc-1".into(),
            doc_type: "persona".into(),
            surface: Some("vault".into()),
            status: "draft".into(),
            priority: None,
            owner: "vault".into(),
            payload: "not json at all".into(),
            archived_at: None,
            created_at: "2026-01-01T00:00:00+00:00".into(),
            updated_at: "2026-01-01T00:00:00+00:00".into(),
        };
        // Never fails -- a malformed payload just yields less text.
        let text = document_search_text(&doc);
        assert_eq!(text, "persona vault");
    }

    #[test]
    fn index_document_writes_derived_text_to_search_index() {
        let (_db, index, _dir) = crate::testutil::test_storage();
        let doc = Document {
            id: "doc-1".into(),
            doc_type: "requirement".into(),
            surface: None,
            status: "draft".into(),
            priority: None,
            owner: "mail".into(),
            payload: r#"{"title":"Thread detail","description":"how threads render"}"#.into(),
            archived_at: None,
            created_at: "2026-01-01T00:00:00+00:00".into(),
            updated_at: "2026-01-01T00:00:00+00:00".into(),
        };

        index_document(&index, &doc).expect("index_document");

        let hits = index
            .search_documents("mail", "thread render", 5)
            .expect("search");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].id, "doc-1");
    }

    #[test]
    fn insert_document_indexed_writes_to_search_index() {
        let (db, index, _dir) = crate::testutil::test_storage();
        let meta = sample_meta("requirement", "mail");
        let payload = r#"{"title":"Thread detail","description":"how threads render"}"#;
        let doc = insert_document_indexed(&db, &index, &meta, payload).expect("insert");

        let hits = index
            .search_documents("mail", "thread render", 5)
            .expect("search");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].id, doc.id);
    }

    #[test]
    fn revise_document_indexed_reindexes_without_duplicating() {
        let (db, index, _dir) = crate::testutil::test_storage();
        let mut meta = sample_meta("requirement", "mail");
        meta.id = Some("FR-REVISE-1");
        // Deliberately disjoint vocabulary between the two revisions (no
        // shared words, including the `doc_type`/`surface` terms
        // `document_search_text` appends -- both revisions carry
        // "requirement") so a hit on the stale query can only come from a
        // ghost entry, never from Tantivy's default OR-combined query
        // parser matching a shared token.
        insert_document_indexed(&db, &index, &meta, r#"{"title":"exclusiveDraftAlpha"}"#)
            .expect("insert");

        revise_document_indexed(
            &db,
            &index,
            "FR-REVISE-1",
            r#"{"title":"distinctFinalBeta"}"#,
        )
        .expect("revise");

        // Old title text is gone -- the revised entry replaced it, not
        // just accumulated alongside it.
        let stale = index
            .search_documents("mail", "exclusiveDraftAlpha", 5)
            .expect("search stale");
        assert!(stale.is_empty(), "stale pre-revision text must not match");

        let hits = index
            .search_documents("mail", "distinctFinalBeta", 5)
            .expect("search fresh");
        assert_eq!(hits.len(), 1, "revise must not duplicate the index entry");
        assert_eq!(hits[0].id, "FR-REVISE-1");
    }

    #[test]
    fn set_document_status_indexed_does_not_duplicate_index_entry() {
        let (db, index, _dir) = crate::testutil::test_storage();
        let mut meta = sample_meta("requirement", "mail");
        meta.id = Some("FR-STATUS-1");
        insert_document_indexed(&db, &index, &meta, r#"{"title":"Status target"}"#)
            .expect("insert");

        set_document_status_indexed(&db, &index, "FR-STATUS-1", "published").expect("set status");

        let hits = index
            .search_documents("mail", "Status target", 5)
            .expect("search");
        assert_eq!(
            hits.len(),
            1,
            "a status change must re-index, not add a second entry"
        );
        assert_eq!(hits[0].id, "FR-STATUS-1");
    }

    #[test]
    fn archive_document_indexed_stays_searchable() {
        let (db, index, _dir) = crate::testutil::test_storage();
        let mut meta = sample_meta("requirement", "mail");
        meta.id = Some("FR-ARCHIVE-1");
        insert_document_indexed(&db, &index, &meta, r#"{"title":"Archived target"}"#)
            .expect("insert");

        let archived = archive_document_indexed(&db, &index, "FR-ARCHIVE-1").expect("archive");
        assert!(archived.archived_at.is_some());

        let hits = index
            .search_documents("mail", "Archived target", 5)
            .expect("search");
        assert_eq!(hits.len(), 1, "an archived document must remain searchable");
        assert_eq!(hits[0].id, "FR-ARCHIVE-1");
    }

    #[test]
    fn get_all_documents_for_reindex_includes_archived_and_hot() {
        let db = test_db();
        let mut hot_meta = sample_meta("requirement", "mail");
        hot_meta.id = Some("FR-HOT-1");
        db.insert_document(&hot_meta, "{}").unwrap();

        let mut archived_meta = sample_meta("requirement", "mail");
        archived_meta.id = Some("FR-ARCHIVED-1");
        db.insert_document(&archived_meta, "{}").unwrap();
        db.archive_document("FR-ARCHIVED-1").unwrap();

        let all = db.get_all_documents_for_reindex().unwrap();
        let ids: Vec<&str> = all.iter().map(|d| d.id.as_str()).collect();
        assert!(ids.contains(&"FR-HOT-1"));
        assert!(
            ids.contains(&"FR-ARCHIVED-1"),
            "reindex must not silently drop archived documents"
        );
    }
}
