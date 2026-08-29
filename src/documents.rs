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

        // #1062: refuse a payload that does not conform to its doc_type's
        // landed schema (or has no schema at all) before any SQL runs.
        let payload_value: serde_json::Value = serde_json::from_str(&payload)
            .map_err(|e| LegionError::WorkSource(format!("payload is not valid JSON: {e}")))?;
        self.validate_document_payload(meta.doc_type, &payload_value)?;

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

        // #1062: validate against the EXISTING row's doc_type -- revise
        // never changes doc_type, so the schema that governed the document
        // before still governs the replacement payload.
        let payload_value: serde_json::Value = serde_json::from_str(&normalized_payload)
            .map_err(|e| LegionError::WorkSource(format!("payload is not valid JSON: {e}")))?;
        self.validate_document_payload(&existing.doc_type, &payload_value)?;

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
        let mut value: serde_json::Value =
            serde_json::from_str(&existing.payload).map_err(|e| {
                LegionError::WorkSource(format!("document '{id}' payload is not valid JSON: {e}"))
            })?;
        if !value.is_object() {
            return Err(LegionError::WorkSource(format!(
                "document '{id}' payload is not a JSON object"
            )));
        }

        // #1062: validate the MERGED payload (existing fields + the new
        // body) against the doc_type's schema before writing. This merge
        // exists only to check conformance -- it is never written back;
        // the UPDATE below still evaluates `json_set` against the row's
        // CURRENT value at UPDATE time (#1036 review, MED-1), not this
        // snapshot, so the read-modify-write window that guard closed
        // stays closed.
        if let Some(obj) = value.as_object_mut() {
            obj.insert(
                "body".to_string(),
                serde_json::Value::String(body.to_string()),
            );
        }
        self.validate_document_payload(&existing.doc_type, &value)?;

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
}

/// Extension keyword on a schema document's payload that declares the
/// `doc_type` it governs (#1062). JSON Schema permits arbitrary "x-"
/// extension keywords and the `jsonschema` crate ignores them when
/// validating instances, so a schema document is self-describing without a
/// dedicated column -- the schema stays portable as a plain file under
/// `schemas/`.
pub const DOC_TYPE_KEYWORD: &str = "x-doc-type";

/// A compiled validator plus the id of the schema document it came from.
/// `schema_id` rides along so a violation names the schema that produced
/// it, keeping the schema document the canonical, citable source of truth
/// for the type.
///
/// Manual `Debug`: `jsonschema::Validator` does not derive it, and tests
/// only need `schema_id` on `unwrap_err`/assertion failure, not the
/// compiled validator's internals.
pub struct TypeSchema {
    pub schema_id: String,
    pub validator: jsonschema::Validator,
}

impl std::fmt::Debug for TypeSchema {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TypeSchema")
            .field("schema_id", &self.schema_id)
            .finish_non_exhaustive()
    }
}

/// Compile `value` as a JSON Schema validator (#1062). `what` names the
/// schema in the error message (e.g. `"schema document '<id>' for type
/// '<type>'"`, or `"stored schema '<id>'"`) so a compile failure -- a
/// `$ref` to a missing definition, a malformed `type`, or any other
/// draft-07/2020-12 violation -- is traceable to its source. Shared by
/// `schema_for_type`, `validate_schema_payload`, and `legion document
/// validate` so all three report a compile failure the same way.
pub fn compile_schema(value: &serde_json::Value, what: &str) -> Result<jsonschema::Validator> {
    jsonschema::validator_for(value)
        .map_err(|e| LegionError::WorkSource(format!("{what} does not compile: {e}")))
}

/// Validate `instance` against `validator`, returning one `<json pointer>:
/// <message>` line per violation (#1062) -- the pointer is empty for a
/// violation rooted at the instance itself (e.g. a missing required
/// top-level property). Shared by `validate_document_payload` and `legion
/// document validate` so a write refusal and an explicit validate call
/// report the same violation in the same form.
pub fn schema_violations(
    validator: &jsonschema::Validator,
    instance: &serde_json::Value,
) -> Vec<String> {
    validator
        .iter_errors(instance)
        .map(|e| format!("{}: {e}", e.instance_path()))
        .collect()
}

/// Cap for both the literal JSON nesting depth and the total `$ref`
/// population a schema payload may carry before `compile_schema` ever sees
/// it (#1062 review, HIGH). `jsonschema` 0.52's compiler walks a schema
/// with unbounded native recursion -- no depth or recursion-limit option
/// anywhere in its public `ValidationOptions` -- so a schema with a few
/// hundred single-hop `$ref` aliases, however they are nested or wherever
/// they point, or a few hundred levels of plain object nesting with no
/// `$ref` at all, overflows the stack. That is a stack-guard-page trap
/// (`SIGABRT`), not a panic -- `catch_unwind` never sees it, so it takes
/// the whole daemon process down regardless of which thread compiles the
/// schema, and no thread-stack size closes it: the measured crash floor
/// (~500 hops, ~18 KB) sits far below the 4 MiB request-body cap, so a
/// bigger stack only raises the bar the attacker's budget still clears.
/// Every real landed schema carries at most three `$ref`s, far under 64 of
/// either.
const MAX_SCHEMA_DEPTH: usize = 64;

/// Guard `root` against the two shapes that crash `jsonschema` 0.52's
/// compiler (#1062 review, HIGH) -- called from `validate_schema_payload`
/// BEFORE `compile_schema`, since every schema document must pass that gate
/// before it can be stored, closing all four `compile_schema` call sites
/// (`schema_for_type`, `validate_schema_payload`, and `legion document
/// validate`, which only ever compiles an already-stored, already-guarded
/// payload) with this one check. Uses an explicit stack throughout --
/// never recursion, since the guard itself must not repeat the bug it
/// exists to catch.
///
/// (a) Literal JSON object/array nesting depth: a plain deeply nested
///     schema with no `$ref` at all crashes the compiler exactly the same
///     way, since the walk that blows the stack is a generic structural
///     one, not `$ref`-specific.
/// (b) Total `$ref` population: a schema whose JSON is shallow -- entries
///     that alias each other into a long dereference chain -- crashes the
///     same way even though the literal nesting is not deep. A prior
///     version of this axis tried to FOLLOW a chain (matching only a bare
///     top-level `{"$ref": "..."}` under a literal `#/definitions/` or
///     `#/$defs/` prefix) and was bypassed by five independent, trivial
///     shapes (#1062 review, HIGH, round 2): a `$ref` one level inside
///     `properties`, inside `items`, inside `allOf`, a `$ref` to a
///     non-`definitions` location (e.g. `#/properties/pN`), and a `$ref`
///     using RFC 6901 escapes the lookup never unescaped -- every one of
///     them dodges the follow-logic's assumptions about where a `$ref`
///     sits and how its pointer is spelled while still crashing the
///     compiler. Modeling every keyword the compiler might walk into
///     (`properties`, `items`, `allOf`/`anyOf`/`oneOf`, `patternProperties`,
///     ...) plus full JSON-Pointer resolution is a losing race against a
///     crate the guard does not control. Counting instead of following
///     sidesteps needing to know any of that: a dereference chain of
///     length N needs at least N distinct `$ref` nodes somewhere in the
///     document, however they are nested, wherever they point, however
///     they are spelled -- so bounding the document's total `$ref` count
///     bounds every possible chain by construction, not by case coverage.
///     A same-document `$ref` cycle (e.g. two definitions aliasing each
///     other) is legal JSON Schema that the crate compiles without
///     unbounded recursion, so it is not specially refused here -- it is
///     bounded by the same count, like any other `$ref` structure.
fn guard_schema_depth(root: &serde_json::Value) -> Result<()> {
    // (a) literal nesting: a generic structural walk with no JSON Schema
    // semantics -- every nested object/array value, regardless of which
    // keyword holds it, contributes one level.
    let mut stack: Vec<(&serde_json::Value, usize)> = vec![(root, 0)];
    while let Some((value, depth)) = stack.pop() {
        if depth > MAX_SCHEMA_DEPTH {
            return Err(LegionError::WorkSource(format!(
                "schema nesting depth exceeds {MAX_SCHEMA_DEPTH} -- refused before compiling \
                 (the validator has no recursion limit; a sufficiently deep schema crashes \
                 the process)"
            )));
        }
        match value {
            serde_json::Value::Object(map) => {
                stack.extend(map.values().map(|v| (v, depth + 1)));
            }
            serde_json::Value::Array(items) => {
                stack.extend(items.iter().map(|v| (v, depth + 1)));
            }
            _ => {}
        }
    }

    // (b) total $ref population: every object anywhere in the document
    // carrying a "$ref" key counts once, regardless of which keyword holds
    // it, what the pointer targets, or how the pointer is spelled.
    let mut ref_count: usize = 0;
    let mut walk: Vec<&serde_json::Value> = vec![root];
    while let Some(value) = walk.pop() {
        match value {
            serde_json::Value::Object(map) => {
                if map.contains_key("$ref") {
                    ref_count += 1;
                }
                walk.extend(map.values());
            }
            serde_json::Value::Array(items) => {
                walk.extend(items.iter());
            }
            _ => {}
        }
    }
    if ref_count > MAX_SCHEMA_DEPTH {
        return Err(LegionError::WorkSource(format!(
            "schema contains {ref_count} \"$ref\" occurrences, exceeding {MAX_SCHEMA_DEPTH} -- \
             refused before compiling (the validator has no recursion limit; a dereference \
             chain of length N needs at least N \"$ref\" nodes, so bounding the total bounds \
             every possible chain)"
        )));
    }

    Ok(())
}

impl Database {
    /// Resolve the schema governing `doc_type` (#1062): the single hot
    /// (non-archived) document with `doc_type = "schema"` whose payload
    /// carries `"x-doc-type": <doc_type>`. Status does not participate in
    /// resolution -- a schema row still marked `draft` governs its type the
    /// same as one marked `adopted` (Sean's ruling, reflection 01a04e9f: no
    /// warn-and-allow, no fallback schema).
    ///
    /// Zero matches is refused, naming the type and what to do about it.
    /// Two or more matches is refused, naming every candidate id -- picking
    /// one silently would make which schema governs a type depend on row
    /// order, which is not a decision this call makes on the caller's
    /// behalf.
    pub fn schema_for_type(&self, doc_type: &str) -> Result<TypeSchema> {
        let schema_docs = self.list_documents(&DocumentFilter {
            doc_type: Some("schema"),
            ..Default::default()
        })?;
        let matches: Vec<&Document> = schema_docs
            .iter()
            .filter(|doc| {
                serde_json::from_str::<serde_json::Value>(&doc.payload)
                    .ok()
                    .and_then(|v| {
                        v.get(DOC_TYPE_KEYWORD)
                            .and_then(|k| k.as_str())
                            .map(str::to_string)
                    })
                    .as_deref()
                    == Some(doc_type)
            })
            .collect();

        match matches.as_slice() {
            [] => Err(LegionError::WorkSource(format!(
                "no schema document declares \"{DOC_TYPE_KEYWORD}\": \"{doc_type}\" -- a schema \
                 document with that keyword must exist before a '{doc_type}' document can be \
                 written (every --doc-type needs a landed schema, no warn-and-allow, no fallback)"
            ))),
            [only] => {
                let schema_value: serde_json::Value =
                    serde_json::from_str(&only.payload).map_err(|e| {
                        LegionError::WorkSource(format!(
                            "schema document '{}' for type '{doc_type}' is not valid JSON: {e}",
                            only.id
                        ))
                    })?;
                let validator = compile_schema(
                    &schema_value,
                    &format!("schema document '{}' for type '{doc_type}'", only.id),
                )?;
                Ok(TypeSchema {
                    schema_id: only.id.clone(),
                    validator,
                })
            }
            many => {
                let ids: Vec<&str> = many.iter().map(|d| d.id.as_str()).collect();
                Err(LegionError::WorkSource(format!(
                    "multiple schema documents declare \"{DOC_TYPE_KEYWORD}\": \"{doc_type}\": {} \
                     -- exactly one must govern a type",
                    ids.join(", ")
                )))
            }
        }
    }

    /// Validate `payload` against the resolved schema for `doc_type`,
    /// called by `insert_document`, `revise_document`, and
    /// `update_document_body` before any SQL runs (#1062).
    ///
    /// Exempt: `doc_type == "schema"`. A schema document's own structural
    /// well-formedness is checked by `validate_schema_payload` at the
    /// CLI/channel boundary (draft-07 shape + a jsonschema compile check),
    /// not by resolving a schema that would have to govern the type
    /// "schema" itself -- that schema cannot exist without circularity.
    /// Concretely: `revise_document` (which this call also gates) is how a
    /// schema document is ever edited, including landing a future schema;
    /// if the generic check applied to `doc_type = "schema"` here, no
    /// schema could ever be revised. (The eleven schema rows this issue
    /// targets already carry `x-doc-type`, added by hand on 2026-08-29 --
    /// this exemption is about the bootstrap circularity and every future
    /// schema landing, not a one-time migration step.)
    pub fn validate_document_payload(
        &self,
        doc_type: &str,
        payload: &serde_json::Value,
    ) -> Result<()> {
        if doc_type == "schema" {
            return Ok(());
        }
        let type_schema = self.schema_for_type(doc_type)?;
        let errors = schema_violations(&type_schema.validator, payload);
        if errors.is_empty() {
            Ok(())
        } else {
            Err(LegionError::SchemaViolation {
                schema_id: type_schema.schema_id,
                errors,
            })
        }
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
    // #1062: the schema must declare the doc_type it governs -- without it
    // `schema_for_type` has nothing to resolve against, and the schema
    // would land as an orphan no write path can ever reach.
    obj.get(DOC_TYPE_KEYWORD)
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            LegionError::WorkSource(format!(
                "schema payload missing required string field '{DOC_TYPE_KEYWORD}' -- a schema \
                 document must declare the doc_type it governs"
            ))
        })?;

    // #1062 review (security, HIGH): refuse a schema shaped to overflow the
    // compiler's native recursion (a long $ref alias chain, or plain deep
    // nesting) BEFORE compile_schema ever walks it -- see
    // guard_schema_depth's doc comment for why neither a bigger thread
    // stack nor catching a panic closes this.
    guard_schema_depth(&value)?;

    // #1062: compile through the crate so a $ref to a missing definition, a
    // malformed 'type', or any other draft-07/2020-12 violation is refused
    // at create/revise -- not silently accepted and discovered only the
    // first time someone tries to validate an instance (or write a
    // document of the governed type) against it.
    compile_schema(&value, "schema")?;

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
        crate::db::testutil::seed_type_schema(&db, "requirement");
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
        crate::db::testutil::seed_type_schema(&db, "persona");
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
        crate::db::testutil::seed_type_schema(&db, "requirement");
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
        crate::db::testutil::seed_type_schema(&db, "requirement");
        crate::db::testutil::seed_type_schema(&db, "persona");
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
        crate::db::testutil::seed_type_schema(&db, "requirement");
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
        crate::db::testutil::seed_type_schema(&db, "requirement");
        let mut m = sample_meta("requirement", "mail");
        m.id = Some("FR-A");
        db.insert_document(&m, "{}").unwrap();
        m.id = Some("FR-B");
        db.insert_document(&m, "{}").unwrap();

        db.archive_document("FR-A").expect("archive");

        // Filtered by type: the seeded stub schema (its own doc_type =
        // "schema") is also a hot row on this db and must not be counted
        // here -- this test is about archived-vs-hot, not about every row.
        let hot = db
            .list_documents(&DocumentFilter {
                doc_type: Some("requirement"),
                ..Default::default()
            })
            .expect("list");
        assert_eq!(hot.len(), 1);
        assert_eq!(hot[0].id, "FR-B");

        let cold = db
            .list_documents(&DocumentFilter {
                doc_type: Some("requirement"),
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
        crate::db::testutil::seed_type_schema(&db, "requirement");
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
        crate::db::testutil::seed_type_schema(&db, "requirement");
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
            "required": ["meta", "identity"],
            "x-doc-type": "persona"
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

    /// #1062: an otherwise-valid schema missing `x-doc-type` is refused,
    /// naming the keyword -- `schema_for_type` has nothing to resolve
    /// against a schema that never declares which type it governs.
    #[test]
    fn schema_payload_rejects_missing_doc_type_keyword() {
        let mut v: serde_json::Value = serde_json::from_str(&minimal_schema()).unwrap();
        v.as_object_mut().unwrap().remove(DOC_TYPE_KEYWORD);
        let err = validate_schema_payload(&v.to_string()).unwrap_err();
        assert!(
            err.to_string().contains(DOC_TYPE_KEYWORD),
            "expected error naming '{DOC_TYPE_KEYWORD}', got: {err}"
        );
    }

    /// #1062: a `$ref` to a missing definition does not compile through the
    /// `jsonschema` crate, and the crate's own compile error rides in the
    /// refusal message.
    #[test]
    fn schema_payload_rejects_uncompilable_ref() {
        let v = serde_json::json!({
            "$schema": "http://json-schema.org/draft-07/schema#",
            "title": "Broken Ref",
            "type": "object",
            "properties": {"x": {"$ref": "#/definitions/missing"}},
            "x-doc-type": "broken-ref"
        });
        let err = validate_schema_payload(&v.to_string()).unwrap_err();
        assert!(err.to_string().contains("does not compile"), "got: {err}");
    }

    // -- guard_schema_depth (#1062 review, HIGH: unbounded native recursion
    // in jsonschema 0.52's compiler crashes the whole process) ------------

    /// A schema whose JSON is shallow (a flat `definitions` map) but whose
    /// entries alias each other in a 600-hop `$ref` chain is refused,
    /// naming the count bound, before it ever reaches `compile_schema`.
    #[test]
    fn schema_payload_rejects_long_ref_chain() {
        const N: usize = 600;
        let mut definitions = serde_json::Map::new();
        for i in 0..N {
            definitions.insert(
                format!("d{i}"),
                serde_json::json!({"$ref": format!("#/definitions/d{}", i + 1)}),
            );
        }
        definitions.insert(format!("d{N}"), serde_json::json!({"type": "string"}));
        let schema = serde_json::json!({
            "$schema": "http://json-schema.org/draft-07/schema#",
            "title": "Long Ref Chain",
            "type": "object",
            "properties": {"x": {"$ref": "#/definitions/d0"}},
            "definitions": definitions,
            "x-doc-type": "long-ref-chain"
        });
        let err = validate_schema_payload(&schema.to_string()).unwrap_err();
        assert!(
            err.to_string().contains("occurrences"),
            "expected the $ref-count bound named, got: {err}"
        );
    }

    /// #1062 review, HIGH round 2: a `$ref` chain nested one level inside
    /// `properties` (`d_i = {"type":"object","properties":{"a":{"$ref":
    /// ...}}}`) bypassed the prior chain-following axis entirely, because
    /// the follow-logic only looked for a bare top-level `$ref`. The
    /// count-based axis does not care where a `$ref` sits, so this is
    /// refused the same as the bare-alias shape.
    #[test]
    fn schema_payload_rejects_ref_chain_nested_in_properties() {
        const N: usize = 600;
        let mut definitions = serde_json::Map::new();
        for i in 0..N {
            definitions.insert(
                format!("d{i}"),
                serde_json::json!({
                    "type": "object",
                    "properties": {"a": {"$ref": format!("#/definitions/d{}", i + 1)}}
                }),
            );
        }
        definitions.insert(format!("d{N}"), serde_json::json!({"type": "string"}));
        let schema = serde_json::json!({
            "$schema": "http://json-schema.org/draft-07/schema#",
            "title": "Ref Chain In Properties",
            "type": "object",
            "properties": {"x": {"$ref": "#/definitions/d0"}},
            "definitions": definitions,
            "x-doc-type": "ref-chain-in-properties"
        });
        let err = validate_schema_payload(&schema.to_string()).unwrap_err();
        assert!(
            err.to_string().contains("occurrences"),
            "expected the $ref-count bound named, got: {err}"
        );
    }

    /// #1062 review, HIGH round 2: the same bypass via `items` instead of
    /// `properties`.
    #[test]
    fn schema_payload_rejects_ref_chain_nested_in_items() {
        const N: usize = 600;
        let mut definitions = serde_json::Map::new();
        for i in 0..N {
            definitions.insert(
                format!("d{i}"),
                serde_json::json!({
                    "type": "array",
                    "items": {"$ref": format!("#/definitions/d{}", i + 1)}
                }),
            );
        }
        definitions.insert(format!("d{N}"), serde_json::json!({"type": "string"}));
        let schema = serde_json::json!({
            "$schema": "http://json-schema.org/draft-07/schema#",
            "title": "Ref Chain In Items",
            "type": "object",
            "properties": {"x": {"$ref": "#/definitions/d0"}},
            "definitions": definitions,
            "x-doc-type": "ref-chain-in-items"
        });
        let err = validate_schema_payload(&schema.to_string()).unwrap_err();
        assert!(
            err.to_string().contains("occurrences"),
            "expected the $ref-count bound named, got: {err}"
        );
    }

    /// #1062 review, HIGH round 2: the same bypass via `allOf`.
    #[test]
    fn schema_payload_rejects_ref_chain_nested_in_all_of() {
        const N: usize = 600;
        let mut definitions = serde_json::Map::new();
        for i in 0..N {
            definitions.insert(
                format!("d{i}"),
                serde_json::json!({"allOf": [{"$ref": format!("#/definitions/d{}", i + 1)}]}),
            );
        }
        definitions.insert(format!("d{N}"), serde_json::json!({"type": "string"}));
        let schema = serde_json::json!({
            "$schema": "http://json-schema.org/draft-07/schema#",
            "title": "Ref Chain In allOf",
            "type": "object",
            "properties": {"x": {"$ref": "#/definitions/d0"}},
            "definitions": definitions,
            "x-doc-type": "ref-chain-in-all-of"
        });
        let err = validate_schema_payload(&schema.to_string()).unwrap_err();
        assert!(
            err.to_string().contains("occurrences"),
            "expected the $ref-count bound named, got: {err}"
        );
    }

    /// #1062 review, HIGH round 2: a `$ref` chain that never touches
    /// `definitions`/`$defs` at all (`p_i = {"$ref": "#/properties/p_{i+1}"}`,
    /// living entirely under `properties`) bypassed the prior axis, which
    /// only recognized a `#/definitions/` or `#/$defs/` prefix. The
    /// count-based axis does not interpret the pointer at all.
    #[test]
    fn schema_payload_rejects_ref_chain_targeting_properties() {
        const N: usize = 600;
        let mut properties = serde_json::Map::new();
        for i in 0..N {
            properties.insert(
                format!("p{i}"),
                serde_json::json!({"$ref": format!("#/properties/p{}", i + 1)}),
            );
        }
        properties.insert(format!("p{N}"), serde_json::json!({"type": "string"}));
        let schema = serde_json::json!({
            "$schema": "http://json-schema.org/draft-07/schema#",
            "title": "Ref Chain Targeting Properties",
            "type": "object",
            "properties": properties,
            "x-doc-type": "ref-chain-targeting-properties"
        });
        let err = validate_schema_payload(&schema.to_string()).unwrap_err();
        assert!(
            err.to_string().contains("occurrences"),
            "expected the $ref-count bound named, got: {err}"
        );
    }

    /// #1062 review, HIGH round 2: a `$ref` chain using RFC 6901
    /// JSON-Pointer escapes (`~1` for `/`) bypassed the prior axis, whose
    /// lookup took the pointer segment verbatim instead of unescaping it.
    /// The count-based axis does not resolve the pointer at all, so the
    /// escaping is irrelevant to it.
    #[test]
    fn schema_payload_rejects_ref_chain_with_json_pointer_escapes() {
        const N: usize = 600;
        let mut definitions = serde_json::Map::new();
        for i in 0..N {
            definitions.insert(
                format!("s/{i}"),
                serde_json::json!({"$ref": format!("#/definitions/s~1{}", i + 1)}),
            );
        }
        definitions.insert(format!("s/{N}"), serde_json::json!({"type": "string"}));
        let schema = serde_json::json!({
            "$schema": "http://json-schema.org/draft-07/schema#",
            "title": "Ref Chain With Escapes",
            "type": "object",
            "properties": {"x": {"$ref": "#/definitions/s~10"}},
            "definitions": definitions,
            "x-doc-type": "ref-chain-with-escapes"
        });
        let err = validate_schema_payload(&schema.to_string()).unwrap_err();
        assert!(
            err.to_string().contains("occurrences"),
            "expected the $ref-count bound named, got: {err}"
        );
    }

    /// A plain schema nested well past the guard's depth cap, with no
    /// `$ref` at all, is refused naming the depth bound -- the same crash,
    /// a different shape, caught by the guard's other axis.
    ///
    /// The nesting is built as a chain of bare single-element arrays
    /// (`[[[...["leaf"]...]]]`), one level of raw JSON nesting per step,
    /// rather than 600 levels of `{"type":"object","properties":{...}}`:
    /// `serde_json::from_str` (which `validate_schema_payload` calls
    /// before this guard ever runs) enforces its own ~128-level recursion
    /// limit while PARSING text, independent of this guard, so a payload
    /// deep enough to double as a `jsonschema` crash case (500+) never
    /// reaches `guard_schema_depth` as text -- it is refused by the parser
    /// first, with a different message. 100 levels clears this guard's
    /// 64-level cap while staying under serde_json's parse-time limit, so
    /// the assertion below actually exercises `guard_schema_depth`, not
    /// the parser.
    #[test]
    fn schema_payload_rejects_deep_nesting() {
        const N: usize = 100;
        let mut inner = serde_json::json!("leaf");
        for _ in 0..N {
            inner = serde_json::Value::Array(vec![inner]);
        }
        let schema = serde_json::json!({
            "$schema": "http://json-schema.org/draft-07/schema#",
            "title": "Deeply Nested",
            "type": "object",
            "properties": {"a": inner},
            "x-doc-type": "deeply-nested"
        });
        let err = validate_schema_payload(&schema.to_string()).unwrap_err();
        assert!(
            err.to_string().contains("nesting depth"),
            "expected the nesting-depth bound named, got: {err}"
        );
    }

    /// A two-node `$ref` cycle is legal JSON Schema (a normal way to write
    /// a recursive shape) and `jsonschema` compiles it without unbounded
    /// recursion (verified empirically: it does not crash), so the guard
    /// does not specially refuse a cycle -- it is bounded by the same
    /// $ref-count axis as any other structure, and two is far under the
    /// cap, so this passes.
    #[test]
    fn schema_payload_accepts_a_ref_cycle_within_the_count_cap() {
        let schema = serde_json::json!({
            "$schema": "http://json-schema.org/draft-07/schema#",
            "title": "Ref Cycle",
            "type": "object",
            "properties": {"x": {"$ref": "#/definitions/a"}},
            "definitions": {
                "a": {"$ref": "#/definitions/b"},
                "b": {"$ref": "#/definitions/a"}
            },
            "x-doc-type": "ref-cycle"
        });
        validate_schema_payload(&schema.to_string())
            .expect("a two-node $ref cycle within the count cap must be accepted");
    }

    /// The guard must not reject a real, valid landed schema -- every real
    /// schema is far under both caps.
    #[test]
    fn schema_payload_guard_accepts_a_real_landed_schema() {
        let path = format!(
            "{}/schemas/requirement.schema.json",
            env!("CARGO_MANIFEST_DIR")
        );
        let payload = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {path}: {e}"));
        validate_schema_payload(&payload).expect("a real landed schema must pass the depth guard");
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

    // -- schema_for_type / validate_document_payload (#1062) ---------------

    /// Insert a landed schema document, with `schema` as its full payload
    /// (already carrying `$schema`, `title`, `type`, `properties`, and its
    /// own `x-doc-type`) -- direct `insert_document` under `doc_type =
    /// "schema"`, which is exempt from the generic per-type check, so this
    /// seeds a schema without going through the CLI/channel
    /// `validate_schema_payload` gate.
    fn land_schema(db: &Database, schema: serde_json::Value) -> String {
        let meta = sample_meta("schema", "legion");
        let doc = db
            .insert_document(&meta, &schema.to_string())
            .expect("insert schema document");
        doc.id
    }

    fn object_schema(extra_properties: serde_json::Value, required: &[&str]) -> serde_json::Value {
        let mut schema = serde_json::json!({
            "$schema": "http://json-schema.org/draft-07/schema#",
            "title": "Test Type",
            "type": "object",
            "properties": extra_properties,
        });
        if !required.is_empty() {
            schema["required"] = serde_json::json!(required);
        }
        schema
    }

    #[test]
    fn schema_for_type_resolves_the_hot_schema_declaring_x_doc_type() {
        let db = test_db();
        let mut schema = object_schema(serde_json::json!({"name": {"type": "string"}}), &[]);
        schema["x-doc-type"] = serde_json::json!("widget");
        let schema_id = land_schema(&db, schema);

        let resolved = db.schema_for_type("widget").expect("resolves");
        assert_eq!(resolved.schema_id, schema_id);
    }

    #[test]
    fn schema_for_type_refuses_when_no_schema_declares_the_type() {
        let db = test_db();
        let err = db.schema_for_type("nonexistent-type").unwrap_err();
        assert!(
            err.to_string().contains("nonexistent-type"),
            "error must name the type, got: {err}"
        );
    }

    /// Two hot schema rows declaring the same `x-doc-type` is refused,
    /// naming both candidate ids -- status does not break the tie.
    #[test]
    fn schema_for_type_refuses_when_two_schemas_declare_the_same_type() {
        let db = test_db();
        let mut first = object_schema(serde_json::json!({"name": {"type": "string"}}), &[]);
        first["x-doc-type"] = serde_json::json!("persona");
        first["title"] = serde_json::json!("Persona A");
        let mut second = object_schema(serde_json::json!({"name": {"type": "string"}}), &[]);
        second["x-doc-type"] = serde_json::json!("persona");
        second["title"] = serde_json::json!("Persona B");
        let id_a = land_schema(&db, first);
        let id_b = land_schema(&db, second);

        let err = db.schema_for_type("persona").unwrap_err();
        assert!(err.to_string().contains(&id_a), "got: {err}");
        assert!(err.to_string().contains(&id_b), "got: {err}");
    }

    /// `doc_type == "schema"` is exempt from the generic per-type check
    /// (bootstrap: no schema could govern the type "schema" itself without
    /// circularity) -- `insert_document` with `doc_type = "schema"` and an
    /// arbitrary payload succeeds even on an empty database with no landed
    /// schema at all.
    #[test]
    fn validate_document_payload_exempts_schema_doc_type() {
        let db = test_db();
        assert!(
            db.validate_document_payload("schema", &serde_json::json!({"anything": true}))
                .is_ok()
        );
        let meta = sample_meta("schema", "legion");
        assert!(db.insert_document(&meta, r#"{"anything":true}"#).is_ok());
    }

    /// Behavior bullet 1: creating a `requirement` whose payload lacks
    /// `traces_to` is refused, naming `traces_to`, and nothing is written.
    #[test]
    fn insert_document_refuses_payload_violating_schema_and_writes_nothing() {
        let db = test_db();
        let mut schema = object_schema(
            serde_json::json!({"traces_to": {"type": "string"}}),
            &["traces_to"],
        );
        schema["x-doc-type"] = serde_json::json!("requirement");
        land_schema(&db, schema);

        let mut meta = sample_meta("requirement", "mail");
        meta.id = Some("FR-NO-TRACE");
        let err = db.insert_document(&meta, r#"{"title":"x"}"#).unwrap_err();
        // `LegionError::SchemaViolation`'s Display is a count-only summary
        // ("N error(s)") -- the per-violation pointer/message lines live in
        // the `errors` field, which the CLI prints separately (Error
        // Handling: "CLI prints each error line to stderr then the
        // summary").
        let LegionError::SchemaViolation { errors, .. } = &err else {
            panic!("expected SchemaViolation, got: {err:?}");
        };
        assert!(
            errors.iter().any(|e| e.contains("traces_to")),
            "got: {errors:?}"
        );
        assert!(db.get_document("FR-NO-TRACE").unwrap().is_none());
    }

    /// Behavior bullet 2: revising a valid requirement with a
    /// `verification.acceptance` that is not an array is refused, naming
    /// the offending pointer, and the revision counter is unchanged.
    #[test]
    fn revise_document_refuses_violating_payload_and_leaves_revision_unchanged() {
        let db = test_db();
        let mut schema = object_schema(
            serde_json::json!({
                "verification": {
                    "type": "object",
                    "properties": {"acceptance": {"type": "array"}}
                }
            }),
            &[],
        );
        schema["x-doc-type"] = serde_json::json!("requirement");
        land_schema(&db, schema);

        let mut meta = sample_meta("requirement", "mail");
        meta.id = Some("FR-REVISE-BAD");
        db.insert_document(&meta, "{}").expect("insert");

        let bad = serde_json::json!({"verification": {"acceptance": "not an array"}}).to_string();
        let err = db.revise_document("FR-REVISE-BAD", &bad).unwrap_err();
        let LegionError::SchemaViolation { errors, .. } = &err else {
            panic!("expected SchemaViolation, got: {err:?}");
        };
        assert!(
            errors
                .iter()
                .any(|e| e.contains("/verification/acceptance")),
            "got: {errors:?}"
        );
        assert_eq!(db.document_revision("FR-REVISE-BAD").unwrap(), 1);
    }

    /// Behavior bullet 3: a body save that keeps the merged payload
    /// conforming is accepted; one that would make it violate the schema
    /// (here, `body` must be a string) is refused, and the write does not
    /// land.
    #[test]
    fn update_document_body_validates_merged_payload_against_schema() {
        let db = test_db();
        let mut schema = object_schema(serde_json::json!({"body": {"type": "string"}}), &[]);
        schema["x-doc-type"] = serde_json::json!("thesis");
        land_schema(&db, schema);

        let mut meta = sample_meta("thesis", "legion");
        meta.id = Some("TH-BODY-SCHEMA");
        db.insert_document(&meta, "{}").expect("insert");

        // A conforming save succeeds.
        db.update_document_body("TH-BODY-SCHEMA", "draft text")
            .expect("conforming body save");

        // update_document_body's `body` argument is always a Rust `&str`,
        // so a type violation must come from elsewhere in the merged
        // payload -- swap the schema for one that requires an unrelated
        // field to prove a merged-payload violation still refuses the
        // write and does not touch the stored payload.
        let mut strict_schema = object_schema(
            serde_json::json!({"body": {"type": "string"}, "must_exist": {"type": "string"}}),
            &["must_exist"],
        );
        strict_schema["x-doc-type"] = serde_json::json!("thesis-strict");
        land_schema(&db, strict_schema);
        // Seed the row directly (bypassing insert_document's own gate, which
        // would rightly also refuse this) to exercise update_document_body's
        // merge-and-validate step in isolation.
        db.conn
            .execute(
                "INSERT INTO documents (id, type, owner, payload, created_at, updated_at) \
                 VALUES ('TH-BODY-STRICT', 'thesis-strict', 'legion', '{}', '2026-01-01T00:00:00+00:00', '2026-01-01T00:00:00+00:00')",
                [],
            )
            .unwrap();
        let err = db
            .update_document_body("TH-BODY-STRICT", "draft text")
            .unwrap_err();
        let LegionError::SchemaViolation { errors, .. } = &err else {
            panic!("expected SchemaViolation, got: {err:?}");
        };
        assert!(
            errors.iter().any(|e| e.contains("must_exist")),
            "got: {errors:?}"
        );
        let after = db.get_document("TH-BODY-STRICT").unwrap().unwrap();
        assert_eq!(after.payload, "{}", "a refused body save must not land");
    }

    /// Behavior bullet 7: `$ref`/`definitions` are honored -- an instance
    /// violating a `$ref`'d definition is refused, naming the pointer into
    /// the array element.
    #[test]
    fn validate_document_payload_honors_ref_and_definitions() {
        let db = test_db();
        let schema = serde_json::json!({
            "$schema": "http://json-schema.org/draft-07/schema#",
            "title": "System Foundations",
            "type": "object",
            "properties": {
                "nodes": {"type": "array", "items": {"$ref": "#/definitions/node"}}
            },
            "definitions": {
                "node": {
                    "type": "object",
                    "required": ["id"],
                    "properties": {"id": {"type": "string"}}
                }
            },
            "x-doc-type": "system-foundations"
        });
        land_schema(&db, schema);

        let payload = serde_json::json!({"nodes": [{}]});
        let err = db
            .validate_document_payload("system-foundations", &payload)
            .unwrap_err();
        let LegionError::SchemaViolation { errors, .. } = &err else {
            panic!("expected SchemaViolation, got: {err:?}");
        };
        assert!(
            errors.iter().any(|e| e.contains("/nodes/0")),
            "got: {errors:?}"
        );
    }

    /// Behavior bullet 9: validation runs on write only. A row that was
    /// already non-conforming before its schema landed (inserted with raw
    /// SQL, bypassing `insert_document`) is still returned unchanged by
    /// `get_document` and `list_documents`.
    #[test]
    fn list_and_get_document_return_non_conforming_existing_rows_unchanged() {
        let db = test_db();
        let mut schema = object_schema(
            serde_json::json!({"traces_to": {"type": "string"}}),
            &["traces_to"],
        );
        schema["x-doc-type"] = serde_json::json!("requirement");
        land_schema(&db, schema);

        db.conn
            .execute(
                "INSERT INTO documents (id, type, owner, payload, created_at, updated_at) \
                 VALUES ('FR-PREEXISTING', 'requirement', 'legion', '{}', '2026-01-01T00:00:00+00:00', '2026-01-01T00:00:00+00:00')",
                [],
            )
            .unwrap();

        let fetched = db
            .get_document("FR-PREEXISTING")
            .expect("get")
            .expect("row returned despite not conforming");
        assert_eq!(fetched.payload, "{}");

        let listed = db
            .list_documents(&DocumentFilter {
                doc_type: Some("requirement"),
                ..Default::default()
            })
            .expect("list");
        assert!(listed.iter().any(|d| d.id == "FR-PREEXISTING"));
    }

    // -- criteria identity + revision (#882 step 1) -------------------------

    /// A fresh document starts at revision 1 -- the migration's DEFAULT and
    /// this insert path agree without a separate backfill.
    #[test]
    fn new_document_starts_at_revision_one() {
        let db = test_db();
        crate::db::testutil::seed_type_schema(&db, "requirement");
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
        crate::db::testutil::seed_type_schema(&db, "requirement");
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
        crate::db::testutil::seed_type_schema(&db, "requirement");
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
        crate::db::testutil::seed_type_schema(&db, "requirement");
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
        crate::db::testutil::seed_type_schema(&db, "requirement");
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
        crate::db::testutil::seed_type_schema(&db, "requirement");
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
        crate::db::testutil::seed_type_schema(&db, "thesis");
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
        crate::db::testutil::seed_type_schema(&db, "thesis");
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
        crate::db::testutil::seed_type_schema(&db, "thesis");
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
        crate::db::testutil::seed_type_schema(&db, "requirement");
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
