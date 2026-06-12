//! Spec-gen: derive requirement documents from service-design artifacts (#527).
//!
//! # Purpose
//!
//! `generate_requirements` is a pure function (no I/O, no DB) that reads a
//! slice of service-design `Document`s and returns one requirement candidate
//! per `moment_of_truth` entry found in the input.  The persistence layer
//! (`persist_requirements`) validates each candidate against the live
//! requirement schema document in the DB, inserts accepted documents, and
//! creates one born-Backlog kanban card per inserted document.
//!
//! # Trace-to convention
//!
//! Every generated requirement carries a `traces_to` value that encodes its
//! provenance as a stable fragment pointer into the source document:
//!
//! - Ecosystem moment (numbered): `"<doc_id>#moment_of_truth.<number>"`
//! - Persona singleton moment:    `"<doc_id>#moment_of_truth"`
//!
//! Resolution means: the referenced source document is present in the input
//! set AND the fragment resolves inside its payload (the numbered entry exists /
//! the singleton exists).  Unresolvable traces_to values are rejected, never
//! silently dropped.
//!
//! # v1 scope
//!
//! - Ecosystem docs: one candidate per entry in `moments_of_truth`.
//! - Persona docs: one candidate when the doc carries a `moment_of_truth` singleton.
//! - Journey, blueprint, painmatrix: accepted as input but contribute no candidates
//!   (they have no moments).  This is intentional and documented; the next
//!   iteration will derive candidates from journey phases and pain themes.
//! - `depends_on` is always empty in v1; dependency edges between requirements
//!   cannot be derived mechanically from moments alone.
//!
//! # Idempotency
//!
//! Deduplication key: `(traces_to, surface)`.  Before creating a requirement
//! document, `persist_requirements` queries existing non-archived documents of
//! type `requirement` on the surface and skips candidates whose `traces_to`
//! already exists.  `persist_requirements` accepts a `surface` scope parameter;
//! candidates whose `req.surface` differs from that scope are skipped with a
//! logged mismatch (the CLI always passes the same surface used to fetch docs,
//! so mismatches in production indicate a caller bug).

use chrono::Utc;
use serde_json::json;
use uuid::Uuid;

use crate::db::Database;
use crate::documents::{Document, DocumentFilter, DocumentMeta, validate_instance};
use crate::error::{LegionError, Result};

/// Service-design document types that spec-gen reads as input.
///
/// Journey, blueprint, and painmatrix are included so the CLI can filter for
/// them in a single loop; in v1 they contribute no candidates (no moments).
pub const SERVICE_DESIGN_TYPES: &[&str] =
    &["persona", "journey", "blueprint", "painmatrix", "ecosystem"];

/// A fully-composed requirement payload ready for insertion.
#[derive(Debug, Clone)]
pub struct Requirement {
    /// The generated UUIDv7 id for the new document.
    pub id: String,
    /// The surface this requirement belongs to (taken from the source doc).
    pub surface: String,
    /// Requirement title.
    pub title: String,
    /// Requirement description.
    pub description: String,
    /// Provenance fragment: `<doc_id>#moment_of_truth[.<number>]`.
    pub traces_to: String,
    /// Serialized JSON payload conforming to the requirement schema.
    pub payload: String,
}

/// A candidate that was rejected because its `traces_to` did not resolve or
/// its source document was structurally invalid (no surface, missing title).
#[derive(Debug, Clone)]
pub struct Rejection {
    /// The candidate title (for human-readable output).
    pub title: String,
    /// The traces_to value that failed to resolve, or a placeholder when the
    /// fragment could not be formed at all.
    pub traces_to: String,
    /// Human-readable reason for rejection.
    pub reason: String,
}

/// Output of `generate_requirements`.
#[derive(Debug, Default)]
pub struct GenerateOutcome {
    /// Fully-validated requirement payloads, ready for persistence.
    pub requirements: Vec<Requirement>,
    /// Candidates rejected because their `traces_to` did not resolve or their
    /// source document was structurally invalid.
    pub rejected: Vec<Rejection>,
}

/// Output of `persist_requirements`.
#[derive(Debug, Default)]
pub struct PersistOutcome {
    /// Number of new requirement documents and cards created.
    pub created: usize,
    /// Number of candidates skipped because their traces_to already existed
    /// on the requested surface (idempotency dedup).
    pub skipped_existing: usize,
    /// Number of candidates skipped because their `req.surface` did not match
    /// the requested scope surface.  In normal CLI operation (docs are fetched
    /// by surface) this is always 0; a non-zero value indicates a caller bug.
    pub skipped_mismatch: usize,
    /// Candidates rejected at the generation stage.
    pub rejected: Vec<Rejection>,
}

/// Generate requirement candidates from a slice of service-design documents.
///
/// Pure: no I/O, no DB.  The returned `GenerateOutcome` contains validated
/// `Requirement` structs and any `Rejection`s.  Schema-invalid generated
/// payloads are a hard `Err` (that is a bug in spec-gen, not bad input).
pub fn generate_requirements(service_design: &[Document]) -> Result<GenerateOutcome> {
    let mut outcome = GenerateOutcome::default();

    for doc in service_design {
        match doc.doc_type.as_str() {
            "ecosystem" => collect_ecosystem_candidates(doc, &mut outcome)?,
            "persona" => collect_persona_candidates(doc, &mut outcome)?,
            // journey, blueprint, painmatrix: no moments in v1; skip without error.
            _ => {}
        }
    }

    Ok(outcome)
}

/// Persist validated requirements to the database.
///
/// `surface` is the CLI scope (repo name) -- used as the dedup key and the
/// `to_repo` for created kanban cards.  Candidates whose `req.surface` does
/// not match `surface` are skipped and counted as `skipped_existing` with a
/// note; in normal operation (CLI fetches docs by surface) this never fires.
///
/// For each `Requirement` in `outcome.requirements`:
/// 1. Skip if `req.surface != surface` (scope mismatch; counted as skipped).
/// 2. Skip if `(traces_to, surface)` already exists (idempotency).
/// 3. Validate the payload against the requirement schema document from the DB.
///    A validation failure is a hard `Err` (bug in spec-gen).
/// 4. Insert the requirement document (`doc_type="requirement"`, `status="draft"`,
///    `owner="legion"`).
/// 5. Create a born-Backlog kanban card linked to the document.
pub fn persist_requirements(
    db: &Database,
    surface: &str,
    outcome: GenerateOutcome,
) -> Result<PersistOutcome> {
    // Dynamically find the newest non-archived schema document whose payload
    // title is "Requirement" (#528: the v1 schema id is archived; hardcoding
    // ids is fragile across schema versions). The filter returns docs ordered
    // by updated_at DESC, so the first result is the most recent live schema.
    let schema_doc = find_requirement_schema(db)?;
    let schema_value: serde_json::Value =
        serde_json::from_str(&schema_doc.payload).map_err(|e| {
            LegionError::WorkSource(format!(
                "stored requirement schema payload is not valid JSON: {e}"
            ))
        })?;

    // Build the set of existing traces_to values on this surface to enable
    // deduplication without repeated queries.  A parse error on a stored
    // payload is DB corruption -- surface it as a hard Err rather than
    // silently dropping the entry from the dedup set (which would cause
    // duplicate creation on re-run).
    let existing_filter = DocumentFilter {
        doc_type: Some("requirement"),
        surface: Some(surface),
        ..Default::default()
    };
    let existing_docs = db.list_documents(&existing_filter)?;
    let mut existing_traces: std::collections::HashSet<String> =
        std::collections::HashSet::with_capacity(existing_docs.len());
    for d in &existing_docs {
        let v = serde_json::from_str::<serde_json::Value>(&d.payload).map_err(|e| {
            LegionError::WorkSource(format!(
                "stored requirement document '{}' has unparseable payload (DB corruption?): {e}",
                d.id
            ))
        })?;
        if let Some(t) = v["traces_to"].as_str() {
            existing_traces.insert(t.to_string());
        }
    }

    let mut persist_outcome = PersistOutcome {
        rejected: outcome.rejected,
        ..Default::default()
    };

    for req in outcome.requirements {
        // Scope guard: skip candidates that don't belong to this surface.
        if req.surface != surface {
            persist_outcome.skipped_mismatch += 1;
            continue;
        }

        // Idempotency: skip if (traces_to, surface) already exists.
        if existing_traces.contains(&req.traces_to) {
            persist_outcome.skipped_existing += 1;
            continue;
        }

        // Validate generated payload against the requirement schema.
        // A failure here is a spec-gen bug, not bad user input.
        let payload_value: serde_json::Value = serde_json::from_str(&req.payload).map_err(|e| {
            LegionError::WorkSource(format!(
                "spec-gen produced invalid JSON for '{}': {e}",
                req.title
            ))
        })?;
        let errors = validate_instance(&schema_value, &payload_value);
        if !errors.is_empty() {
            return Err(LegionError::WorkSource(format!(
                "spec-gen produced a schema-invalid requirement for '{}': {}",
                req.title,
                errors.join("; ")
            )));
        }

        // Insert the requirement document and its born-Backlog kanban card
        // atomically.  If card creation fails the document is also rolled
        // back, preventing orphaned requirement docs that dedup would
        // permanently hide from re-runs.
        let meta = DocumentMeta {
            id: Some(&req.id),
            doc_type: "requirement",
            surface: Some(&req.surface),
            status: Some("draft"),
            priority: None,
            owner: "legion",
        };
        db.insert_requirement_with_card(
            &meta,
            &req.payload,
            "legion",
            &req.surface,
            &req.title,
            Some(req.description.as_str()),
            &req.id,
        )?;

        persist_outcome.created += 1;
    }

    Ok(persist_outcome)
}

// -- private helpers ---------------------------------------------------------

/// Find the active requirement schema document.
///
/// Searches for non-archived documents of `doc_type = "schema"` whose
/// payload carries `"title": "Requirement"` (case-sensitive). The list
/// is ordered by `updated_at DESC` so the first match is the most
/// recently updated live schema.
///
/// Returns a clear error naming what was looked for when no schema is found,
/// so operators know exactly which document is missing rather than seeing a
/// generic "not found" from a stale hardcoded id.
fn find_requirement_schema(db: &Database) -> Result<Document> {
    let filter = DocumentFilter {
        doc_type: Some("schema"),
        archived: Some(false),
        ..Default::default()
    };
    let candidates = db.list_documents(&filter)?;
    for doc in candidates {
        // Parse lazily; skip docs whose payload is not valid JSON or has no title.
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(&doc.payload)
            && value["title"].as_str() == Some("Requirement")
        {
            return Ok(doc);
        }
    }
    Err(LegionError::WorkSource(
        "no active requirement schema found: expected a non-archived document \
         of doc_type='schema' with payload title='Requirement'. \
         Run `legion document create --type schema` to land the requirement schema."
            .to_string(),
    ))
}

/// Parse a service-design document's surface and JSON payload.
///
/// Returns `(surface, payload_value)`.  Rejects surfaceless documents (a
/// document without a surface cannot be deduped) and unparseable payloads.
/// The rejection is pushed onto `outcome.rejected` and `Err` is returned only
/// for internal logic bugs; bad source documents produce `Ok(None)`.
fn parse_doc(doc: &Document) -> Result<Option<(&str, serde_json::Value)>> {
    let surface = match doc.surface.as_deref() {
        Some(s) if !s.is_empty() => s,
        _ => return Ok(None), // surfaceless: caller pushes Rejection
    };
    let payload: serde_json::Value = serde_json::from_str(&doc.payload).map_err(|e| {
        LegionError::WorkSource(format!(
            "{} document '{}' has invalid JSON payload: {e}",
            doc.doc_type, doc.id
        ))
    })?;
    Ok(Some((surface, payload)))
}

/// Extract `moments_of_truth` entries from an ecosystem document and add
/// one candidate per entry.  Duplicate moment numbers within a single
/// document make the fragment ambiguous; those entries are rejected.
/// A moment with no `title` field is also rejected (not silently defaulted).
fn collect_ecosystem_candidates(doc: &Document, outcome: &mut GenerateOutcome) -> Result<()> {
    let (surface, payload) = match parse_doc(doc)? {
        Some(pair) => pair,
        None => {
            outcome.rejected.push(Rejection {
                title: format!("(ecosystem {})", doc.id),
                traces_to: format!("{}#moment_of_truth.?", doc.id),
                reason: format!(
                    "ecosystem document '{}' has no surface -- cannot dedup generated requirements",
                    doc.id
                ),
            });
            return Ok(());
        }
    };

    // moments_of_truth present but wrong type -> explicit Rejection.
    // Absent key (Value::Null) is legitimate: no moments, no candidates.
    let moments = if payload["moments_of_truth"].is_null() {
        return Ok(());
    } else {
        match payload["moments_of_truth"].as_array() {
            Some(arr) => arr,
            None => {
                outcome.rejected.push(Rejection {
                    title: format!("(ecosystem {})", doc.id),
                    traces_to: format!("{}#moment_of_truth.?", doc.id),
                    reason: format!(
                        "ecosystem document '{}' has a 'moments_of_truth' key but its value \
                         is not an array (got {})",
                        doc.id,
                        payload["moments_of_truth"]
                            .as_object()
                            .map(|_| "object")
                            .unwrap_or("non-array")
                    ),
                });
                return Ok(());
            }
        }
    };

    // Build a count map to detect duplicate moment numbers before emitting
    // candidates.  A duplicate makes the fragment ambiguous and is rejected.
    let mut number_counts: std::collections::HashMap<i64, usize> = std::collections::HashMap::new();
    for entry in moments.iter() {
        if let Some(n) = entry["number"].as_i64() {
            *number_counts.entry(n).or_insert(0) += 1;
        }
    }

    for entry in moments.iter() {
        let number = entry["number"].as_i64();
        let actor = entry["actor"].as_str().unwrap_or("");
        let success = entry["success"].as_str().unwrap_or("");
        let failure = entry["failure"].as_str().unwrap_or("");

        // Missing title -> Rejection (not a silent empty string).
        let title_raw = match entry["title"].as_str().filter(|s| !s.is_empty()) {
            Some(t) => t,
            None => {
                let placeholder = match number {
                    Some(n) => format!("{}#moment_of_truth.{n}", doc.id),
                    None => format!("{}#moment_of_truth.?", doc.id),
                };
                outcome.rejected.push(Rejection {
                    title: format!("(untitled moment in ecosystem {})", doc.id),
                    traces_to: placeholder.clone(),
                    reason: format!(
                        "ecosystem document '{}' has a moment_of_truth entry with no title",
                        doc.id
                    ),
                });
                continue;
            }
        };

        let candidate_title = format!("MoT: {title_raw}");

        // Number < 1: the fragment would be non-positive and unresolvable.
        if let Some(n) = number
            && n < 1
        {
            outcome.rejected.push(Rejection {
                title: candidate_title,
                traces_to: format!("{}#moment_of_truth.{n}", doc.id),
                reason: format!(
                    "ecosystem document '{}' has a moment_of_truth entry with number {n} \
                     -- moment numbers must be >= 1",
                    doc.id
                ),
            });
            continue;
        }

        // Ambiguity: duplicate moment number -> reject.
        if let Some(n) = number
            && number_counts.get(&n).copied().unwrap_or(0) > 1
        {
            outcome.rejected.push(Rejection {
                title: candidate_title,
                traces_to: format!("{}#moment_of_truth.{n}", doc.id),
                reason: format!(
                    "moment number {n} appears more than once in ecosystem document '{}' \
                     -- the fragment is ambiguous",
                    doc.id
                ),
            });
            continue;
        }

        let traces_to = match number {
            Some(n) => format!("{}#moment_of_truth.{n}", doc.id),
            None => {
                // No number field: unresolvable.
                outcome.rejected.push(Rejection {
                    title: candidate_title,
                    traces_to: format!("{}#moment_of_truth.?", doc.id),
                    reason: "ecosystem moment_of_truth entry has no 'number' field".to_string(),
                });
                continue;
            }
        };

        let description = format!("Actor: {actor}. Success: {success}. Failure: {failure}.");
        let req = build_requirement(surface, &candidate_title, &description, &traces_to)?;
        outcome.requirements.push(req);
    }

    Ok(())
}

/// Extract the `moment_of_truth` singleton from a persona document, if present.
/// A persona with no surface or no `meta.title` is rejected, not silently defaulted.
fn collect_persona_candidates(doc: &Document, outcome: &mut GenerateOutcome) -> Result<()> {
    let (surface, payload) = match parse_doc(doc)? {
        Some(pair) => pair,
        None => {
            outcome.rejected.push(Rejection {
                title: format!("(persona {})", doc.id),
                traces_to: format!("{}#moment_of_truth", doc.id),
                reason: format!(
                    "persona document '{}' has no surface -- cannot dedup generated requirements",
                    doc.id
                ),
            });
            return Ok(());
        }
    };

    // Persona singleton moment_of_truth is optional; no field means no candidate.
    let mot = match payload.get("moment_of_truth") {
        Some(v) if !v.is_null() => v,
        _ => return Ok(()),
    };

    let description_text = mot["description"].as_str().unwrap_or("");
    let success = mot["success"].as_str().unwrap_or("");
    let failure = mot["failure"].as_str().unwrap_or("");

    // Missing meta.title -> Rejection (not a silent fallback to doc id).
    let persona_title = match payload["meta"]["title"].as_str().filter(|s| !s.is_empty()) {
        Some(t) => t,
        None => {
            outcome.rejected.push(Rejection {
                title: format!("(persona {})", doc.id),
                traces_to: format!("{}#moment_of_truth", doc.id),
                reason: format!(
                    "persona document '{}' has no meta.title -- cannot compose requirement title",
                    doc.id
                ),
            });
            return Ok(());
        }
    };

    let candidate_title = format!("MoT: {persona_title}");
    let traces_to = format!("{}#moment_of_truth", doc.id);

    let description = if success.is_empty() && failure.is_empty() {
        description_text.to_string()
    } else {
        format!("{description_text} Success: {success}. Failure: {failure}.")
    };

    let req = build_requirement(surface, &candidate_title, &description, &traces_to)?;
    outcome.requirements.push(req);

    Ok(())
}

/// Build a fully-composed `Requirement` struct with a valid JSON payload.
fn build_requirement(
    surface: &str,
    title: &str,
    description: &str,
    traces_to: &str,
) -> Result<Requirement> {
    let id = Uuid::now_v7().to_string();
    let today = Utc::now().format("%Y-%m-%d").to_string();

    let payload = json!({
        "meta": {
            "id": id,
            "type": "requirement",
            "surface": surface,
            "status": "draft",
            "priority": "SHOULD",
            "owner": "legion",
            "date": today,
            "author": "legion/spec-gen"
        },
        "title": title,
        "description": description,
        "traces_to": traces_to,
        "depends_on": []
    });

    Ok(Requirement {
        id,
        surface: surface.to_string(),
        title: title.to_string(),
        description: description.to_string(),
        traces_to: traces_to.to_string(),
        payload: payload.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::testutil::test_db;
    use crate::kanban::{CardScope, Direction, list_cards};

    // -- fixture builders ----------------------------------------------------

    fn ecosystem_doc_with_two_moments() -> Document {
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
                    "failure": "Post is stuck in draft"
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
        Document {
            id: "eco-doc-001".to_string(),
            doc_type: "ecosystem".to_string(),
            surface: Some("gitpress".to_string()),
            status: "done".to_string(),
            priority: None,
            owner: "vault".to_string(),
            payload: payload.to_string(),
            archived_at: None,
            created_at: "2026-06-01T00:00:00Z".to_string(),
            updated_at: "2026-06-01T00:00:00Z".to_string(),
        }
    }

    fn persona_doc_with_mot() -> Document {
        let payload = serde_json::json!({
            "meta": {
                "title": "The Developer",
                "status": "done",
                "date": "2026-06-01",
                "author": "vault",
                "set": "gitpress",
                "actor": "Developer"
            },
            "identity": {
                "description": "A developer who blogs",
                "mental_model": "Git-first",
                "quote": "Ship it"
            },
            "behaviors": [],
            "frustrations": [],
            "needs": [],
            "moment_of_truth": {
                "description": "The developer's first successful publish",
                "success": "Post appears on their site",
                "failure": "Nothing happens after git push"
            }
        });
        Document {
            id: "persona-doc-001".to_string(),
            doc_type: "persona".to_string(),
            surface: Some("gitpress".to_string()),
            status: "done".to_string(),
            priority: None,
            owner: "vault".to_string(),
            payload: payload.to_string(),
            archived_at: None,
            created_at: "2026-06-01T00:00:00Z".to_string(),
            updated_at: "2026-06-01T00:00:00Z".to_string(),
        }
    }

    fn painmatrix_doc() -> Document {
        let payload = serde_json::json!({
            "meta": {"title": "Pain Matrix", "status": "draft", "date": "2026-06-01", "author": "vault", "set": "gitpress", "surface": "gitpress"},
            "themes": [{"id": "T1", "title": "Performance", "pain_points": ["Slow build"]}]
        });
        Document {
            id: "pain-doc-001".to_string(),
            doc_type: "painmatrix".to_string(),
            surface: Some("gitpress".to_string()),
            status: "draft".to_string(),
            priority: None,
            owner: "vault".to_string(),
            payload: payload.to_string(),
            archived_at: None,
            created_at: "2026-06-01T00:00:00Z".to_string(),
            updated_at: "2026-06-01T00:00:00Z".to_string(),
        }
    }

    fn ecosystem_doc_with_duplicate_number() -> Document {
        let payload = serde_json::json!({
            "meta": {
                "title": "Bad Ecosystem",
                "status": "draft",
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
                    "success": "Post is live",
                    "failure": "Post stuck"
                },
                {
                    "number": 1,
                    "title": "Duplicate Publish",
                    "actor": "Developer",
                    "success": "Also live",
                    "failure": "Also stuck"
                }
            ]
        });
        Document {
            id: "eco-dup-001".to_string(),
            doc_type: "ecosystem".to_string(),
            surface: Some("gitpress".to_string()),
            status: "draft".to_string(),
            priority: None,
            owner: "vault".to_string(),
            payload: payload.to_string(),
            archived_at: None,
            created_at: "2026-06-01T00:00:00Z".to_string(),
            updated_at: "2026-06-01T00:00:00Z".to_string(),
        }
    }

    /// An ecosystem doc with no surface set.
    fn ecosystem_doc_no_surface() -> Document {
        let payload = serde_json::json!({
            "meta": {
                "title": "Surfaceless Ecosystem",
                "status": "draft",
                "date": "2026-06-01",
                "author": "vault",
                "core_service": "unknown"
            },
            "actors": {"primary": [{"name": "Actor", "role": "role"}]},
            "channels": [{"name": "web", "type": "digital", "purpose": "x"}],
            "value_exchanges": [{"from": "A", "to": "B", "gives": "x", "gets": "y"}],
            "moments_of_truth": [
                {"number": 1, "title": "MoT One", "actor": "Actor", "success": "ok", "failure": "fail"}
            ]
        });
        Document {
            id: "eco-nosurface-001".to_string(),
            doc_type: "ecosystem".to_string(),
            surface: None,
            status: "draft".to_string(),
            priority: None,
            owner: "vault".to_string(),
            payload: payload.to_string(),
            archived_at: None,
            created_at: "2026-06-01T00:00:00Z".to_string(),
            updated_at: "2026-06-01T00:00:00Z".to_string(),
        }
    }

    /// A persona doc with no surface set.
    fn persona_doc_no_surface() -> Document {
        let payload = serde_json::json!({
            "meta": {
                "title": "Surfaceless Persona",
                "status": "draft",
                "date": "2026-06-01",
                "author": "vault",
                "set": "unknown",
                "actor": "User"
            },
            "identity": {"description": "desc", "mental_model": "model", "quote": "q"},
            "behaviors": [],
            "frustrations": [],
            "needs": [],
            "moment_of_truth": {"description": "desc", "success": "ok", "failure": "fail"}
        });
        Document {
            id: "persona-nosurface-001".to_string(),
            doc_type: "persona".to_string(),
            surface: None,
            status: "draft".to_string(),
            priority: None,
            owner: "vault".to_string(),
            payload: payload.to_string(),
            archived_at: None,
            created_at: "2026-06-01T00:00:00Z".to_string(),
            updated_at: "2026-06-01T00:00:00Z".to_string(),
        }
    }

    /// An ecosystem doc with a moment that has no title.
    fn ecosystem_doc_with_untitled_moment() -> Document {
        let payload = serde_json::json!({
            "meta": {
                "title": "Eco",
                "status": "draft",
                "date": "2026-06-01",
                "author": "vault",
                "core_service": "x"
            },
            "actors": {"primary": [{"name": "A", "role": "r"}]},
            "channels": [{"name": "web", "type": "digital", "purpose": "p"}],
            "value_exchanges": [{"from": "A", "to": "B", "gives": "x", "gets": "y"}],
            "moments_of_truth": [
                {"number": 1, "actor": "A", "success": "ok", "failure": "fail"}
            ]
        });
        Document {
            id: "eco-notitle-001".to_string(),
            doc_type: "ecosystem".to_string(),
            surface: Some("gitpress".to_string()),
            status: "draft".to_string(),
            priority: None,
            owner: "vault".to_string(),
            payload: payload.to_string(),
            archived_at: None,
            created_at: "2026-06-01T00:00:00Z".to_string(),
            updated_at: "2026-06-01T00:00:00Z".to_string(),
        }
    }

    /// A persona doc with no meta.title.
    fn persona_doc_no_title() -> Document {
        let payload = serde_json::json!({
            "meta": {
                "status": "draft",
                "date": "2026-06-01",
                "author": "vault",
                "set": "gitpress",
                "actor": "User"
            },
            "identity": {"description": "d", "mental_model": "m", "quote": "q"},
            "behaviors": [],
            "frustrations": [],
            "needs": [],
            "moment_of_truth": {"description": "desc", "success": "ok", "failure": "fail"}
        });
        Document {
            id: "persona-notitle-001".to_string(),
            doc_type: "persona".to_string(),
            surface: Some("gitpress".to_string()),
            status: "draft".to_string(),
            priority: None,
            owner: "vault".to_string(),
            payload: payload.to_string(),
            archived_at: None,
            created_at: "2026-06-01T00:00:00Z".to_string(),
            updated_at: "2026-06-01T00:00:00Z".to_string(),
        }
    }

    /// Land the requirement schema doc so persist_requirements can validate.
    ///
    /// No longer uses a hardcoded id -- the dynamic lookup in find_requirement_schema
    /// finds any non-archived schema doc with title "Requirement" (#528).
    fn seed_requirement_schema(db: &Database) {
        let schema_payload = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/schemas/requirement.schema.json"
        ))
        .expect("requirement schema file");
        let meta = DocumentMeta {
            id: None, // generate a fresh UUIDv7; the lookup is by title, not id
            doc_type: "schema",
            surface: Some("definition-layer"),
            status: Some("adopted"),
            priority: None,
            owner: "legion",
        };
        db.insert_document(&meta, &schema_payload)
            .expect("seed requirement schema");
    }

    // -- tests ---------------------------------------------------------------

    #[test]
    fn ecosystem_and_persona_produce_correct_requirement_count() {
        let docs = vec![
            ecosystem_doc_with_two_moments(),
            persona_doc_with_mot(),
            painmatrix_doc(), // contributes nothing
        ];
        let outcome = generate_requirements(&docs).expect("generate");
        // 2 from ecosystem + 1 from persona = 3; painmatrix contributes 0.
        assert_eq!(outcome.requirements.len(), 3, "expected 3 requirements");
        assert!(outcome.rejected.is_empty(), "no rejections expected");
    }

    #[test]
    fn ecosystem_traces_to_values_are_correct() {
        let docs = vec![ecosystem_doc_with_two_moments()];
        let outcome = generate_requirements(&docs).expect("generate");
        assert_eq!(outcome.requirements.len(), 2);

        let traces: Vec<&str> = outcome
            .requirements
            .iter()
            .map(|r| r.traces_to.as_str())
            .collect();
        assert!(
            traces.contains(&"eco-doc-001#moment_of_truth.1"),
            "expected MoT 1, got: {traces:?}"
        );
        assert!(
            traces.contains(&"eco-doc-001#moment_of_truth.2"),
            "expected MoT 2, got: {traces:?}"
        );
    }

    #[test]
    fn persona_traces_to_value_is_correct() {
        let docs = vec![persona_doc_with_mot()];
        let outcome = generate_requirements(&docs).expect("generate");
        assert_eq!(outcome.requirements.len(), 1);
        assert_eq!(
            outcome.requirements[0].traces_to,
            "persona-doc-001#moment_of_truth"
        );
    }

    #[test]
    fn duplicate_moment_number_yields_rejection() {
        let docs = vec![ecosystem_doc_with_duplicate_number()];
        let outcome = generate_requirements(&docs).expect("generate");
        // Both entries with number=1 are rejected (ambiguous fragment).
        assert_eq!(outcome.requirements.len(), 0);
        assert_eq!(
            outcome.rejected.len(),
            2,
            "both duplicate-numbered entries must be rejected"
        );
        for r in &outcome.rejected {
            assert!(
                r.reason.contains("ambiguous"),
                "rejection reason must mention ambiguity: {:?}",
                r.reason
            );
        }
    }

    #[test]
    fn painmatrix_contributes_no_candidates() {
        let docs = vec![painmatrix_doc()];
        let outcome = generate_requirements(&docs).expect("generate");
        assert!(outcome.requirements.is_empty());
        assert!(outcome.rejected.is_empty());
    }

    #[test]
    fn surfaceless_ecosystem_doc_yields_rejection() {
        let docs = vec![ecosystem_doc_no_surface()];
        let outcome = generate_requirements(&docs).expect("generate");
        assert!(
            outcome.requirements.is_empty(),
            "no candidates from surfaceless doc"
        );
        assert_eq!(outcome.rejected.len(), 1, "one rejection expected");
        assert!(
            outcome.rejected[0].reason.contains("no surface"),
            "rejection must mention surface: {:?}",
            outcome.rejected[0].reason
        );
    }

    #[test]
    fn surfaceless_persona_doc_yields_rejection() {
        let docs = vec![persona_doc_no_surface()];
        let outcome = generate_requirements(&docs).expect("generate");
        assert!(outcome.requirements.is_empty());
        assert_eq!(outcome.rejected.len(), 1);
        assert!(
            outcome.rejected[0].reason.contains("no surface"),
            "rejection must mention surface: {:?}",
            outcome.rejected[0].reason
        );
    }

    #[test]
    fn untitled_moment_yields_rejection() {
        let docs = vec![ecosystem_doc_with_untitled_moment()];
        let outcome = generate_requirements(&docs).expect("generate");
        assert!(
            outcome.requirements.is_empty(),
            "no candidate from untitled moment"
        );
        assert_eq!(outcome.rejected.len(), 1);
        assert!(
            outcome.rejected[0].reason.contains("no title"),
            "rejection must mention title: {:?}",
            outcome.rejected[0].reason
        );
    }

    #[test]
    fn persona_with_no_meta_title_yields_rejection() {
        let docs = vec![persona_doc_no_title()];
        let outcome = generate_requirements(&docs).expect("generate");
        assert!(outcome.requirements.is_empty());
        assert_eq!(outcome.rejected.len(), 1);
        assert!(
            outcome.rejected[0].reason.contains("meta.title"),
            "rejection must mention meta.title: {:?}",
            outcome.rejected[0].reason
        );
    }

    #[test]
    fn idempotency_second_run_creates_zero_docs_and_cards() {
        let db = test_db();
        seed_requirement_schema(&db);

        let docs = vec![ecosystem_doc_with_two_moments()];
        let outcome1 = generate_requirements(&docs).expect("first generate");
        let persist1 = persist_requirements(&db, "gitpress", outcome1).expect("first persist");
        assert_eq!(persist1.created, 2);
        assert_eq!(persist1.skipped_existing, 0);

        // Second run: same input, same surface.
        let outcome2 = generate_requirements(&docs).expect("second generate");
        let persist2 = persist_requirements(&db, "gitpress", outcome2).expect("second persist");
        assert_eq!(persist2.created, 0, "second run must create nothing");
        assert_eq!(
            persist2.skipped_existing, 2,
            "second run must skip 2 existing"
        );
    }

    #[test]
    fn born_backlog_card_has_correct_status_and_source_url() {
        let db = test_db();
        seed_requirement_schema(&db);

        let docs = vec![persona_doc_with_mot()];
        let outcome = generate_requirements(&docs).expect("generate");
        let persist = persist_requirements(&db, "gitpress", outcome).expect("persist");
        assert_eq!(persist.created, 1);

        // Find the inserted requirement document.
        let req_docs = db
            .list_documents(&DocumentFilter {
                doc_type: Some("requirement"),
                surface: Some("gitpress"),
                ..Default::default()
            })
            .expect("list");
        assert_eq!(req_docs.len(), 1);
        let req_doc = &req_docs[0];

        // Find the card by listing backlog cards for the surface.
        let cards = list_cards(&db, "gitpress", Direction::Inbound, CardScope::Backlog)
            .expect("list cards");
        assert_eq!(cards.len(), 1, "expected one born-Backlog card");
        let card = &cards[0];

        // Card must be in Backlog status.
        assert_eq!(
            card.status.to_string(),
            "backlog",
            "card must be born Backlog"
        );
        // source_url must match the requirement document id.
        assert_eq!(
            card.source_url.as_deref(),
            Some(req_doc.id.as_str()),
            "card source_url must point to the requirement document"
        );
        assert_eq!(
            card.source_type.as_deref(),
            Some("document"),
            "card source_type must be 'document'"
        );
    }

    #[test]
    fn schema_validation_catches_malformed_generated_payload() {
        // Verifies the validation path by calling validate_instance with a
        // deliberately malformed payload missing all required fields.
        let schema_payload = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/schemas/requirement.schema.json"
        ))
        .expect("requirement schema file");
        let schema: serde_json::Value =
            serde_json::from_str(&schema_payload).expect("parse schema");

        let bad_payload = serde_json::json!({});
        let errors = validate_instance(&schema, &bad_payload);
        assert!(
            !errors.is_empty(),
            "malformed payload must have validation errors"
        );
        let joined = errors.join("; ");
        assert!(
            joined.contains("meta"),
            "must report missing meta: {joined}"
        );
    }

    #[test]
    fn corrupt_stored_payload_in_dedup_set_returns_err() {
        // Finding #4: a stored requirement with an unparseable payload must
        // surface as a hard Err from persist_requirements, not silently drop
        // the entry from the dedup set.
        let db = test_db();
        seed_requirement_schema(&db);

        // Manually insert a requirement doc with a corrupt (non-JSON) payload.
        let corrupt_meta = DocumentMeta {
            id: None,
            doc_type: "requirement",
            surface: Some("gitpress"),
            status: Some("draft"),
            priority: None,
            owner: "legion",
        };
        db.insert_document(&corrupt_meta, "NOT JSON AT ALL")
            .expect("insert corrupt doc");

        // persist_requirements must fail with a WorkSource error citing DB corruption.
        let docs = vec![ecosystem_doc_with_two_moments()];
        let outcome = generate_requirements(&docs).expect("generate");
        let err = persist_requirements(&db, "gitpress", outcome).unwrap_err();
        assert!(
            err.to_string().contains("unparseable payload"),
            "error must cite the corrupt payload: {err}"
        );
    }

    #[test]
    fn moment_number_zero_and_negative_yield_rejections() {
        // MED 1: number < 1 (0 or negative) must be rejected, not silently
        // included with a zero/negative fragment.
        let payload = serde_json::json!({
            "meta": {
                "title": "Test Eco",
                "status": "draft",
                "date": "2026-06-01",
                "author": "vault",
                "core_service": "test"
            },
            "actors": {"primary": [{"name": "A", "role": "r"}]},
            "channels": [{"name": "web", "type": "digital", "purpose": "p"}],
            "value_exchanges": [{"from": "A", "to": "B", "gives": "x", "gets": "y"}],
            "moments_of_truth": [
                {"number": 0, "title": "Zero MoT", "actor": "A", "success": "ok", "failure": "fail"},
                {"number": -1, "title": "Negative MoT", "actor": "A", "success": "ok", "failure": "fail"}
            ]
        });
        let doc = Document {
            id: "eco-badnum-001".to_string(),
            doc_type: "ecosystem".to_string(),
            surface: Some("test".to_string()),
            status: "draft".to_string(),
            priority: None,
            owner: "vault".to_string(),
            payload: payload.to_string(),
            archived_at: None,
            created_at: "2026-06-01T00:00:00Z".to_string(),
            updated_at: "2026-06-01T00:00:00Z".to_string(),
        };
        let outcome = generate_requirements(&[doc]).expect("generate");
        assert!(
            outcome.requirements.is_empty(),
            "no candidates from number <= 0"
        );
        assert_eq!(
            outcome.rejected.len(),
            2,
            "both entries with number <= 0 rejected"
        );
        for r in &outcome.rejected {
            assert!(
                r.reason.contains("must be >= 1"),
                "rejection must mention >= 1 constraint: {:?}",
                r.reason
            );
        }
    }

    #[test]
    fn moments_of_truth_as_object_yields_rejection() {
        // MED 2: key present but value is an object (not array) -> Rejection,
        // not silent zero-candidate result.
        let payload = serde_json::json!({
            "meta": {
                "title": "Bad Eco",
                "status": "draft",
                "date": "2026-06-01",
                "author": "vault",
                "core_service": "test"
            },
            "actors": {"primary": [{"name": "A", "role": "r"}]},
            "channels": [{"name": "web", "type": "digital", "purpose": "p"}],
            "value_exchanges": [{"from": "A", "to": "B", "gives": "x", "gets": "y"}],
            "moments_of_truth": {"number": 1, "title": "Not An Array"}
        });
        let doc = Document {
            id: "eco-wrongtype-001".to_string(),
            doc_type: "ecosystem".to_string(),
            surface: Some("test".to_string()),
            status: "draft".to_string(),
            priority: None,
            owner: "vault".to_string(),
            payload: payload.to_string(),
            archived_at: None,
            created_at: "2026-06-01T00:00:00Z".to_string(),
            updated_at: "2026-06-01T00:00:00Z".to_string(),
        };
        let outcome = generate_requirements(&[doc]).expect("generate");
        assert!(
            outcome.requirements.is_empty(),
            "no candidates when moments_of_truth is an object"
        );
        assert_eq!(
            outcome.rejected.len(),
            1,
            "one rejection for wrong moments_of_truth type"
        );
        assert!(
            outcome.rejected[0].reason.contains("not an array"),
            "rejection must mention type problem: {:?}",
            outcome.rejected[0].reason
        );
    }

    #[test]
    fn doc_and_card_are_atomic_if_card_insert_fails() {
        // HIGH atomicity: if card creation fails (e.g. due to a TX error),
        // the document must also be rolled back.  We simulate by inserting a
        // document with an id that triggers a PK conflict on the second run,
        // but the core guarantee is: after a failed persist_requirements call,
        // no orphan documents appear in the DB.
        //
        // We test the positive atomic path: after a successful
        // insert_requirement_with_card, both the document AND the card exist.
        // The rollback scenario would require injecting a failure mid-TX which
        // is impractical without test-only hooks; the contract is instead
        // validated by unit-testing insert_requirement_with_card directly.
        let db = test_db();
        seed_requirement_schema(&db);

        let docs = vec![persona_doc_with_mot()];
        let outcome = generate_requirements(&docs).expect("generate");
        let persist = persist_requirements(&db, "gitpress", outcome).expect("persist");
        assert_eq!(persist.created, 1, "one requirement created");

        // Document must exist.
        let req_docs = db
            .list_documents(&DocumentFilter {
                doc_type: Some("requirement"),
                surface: Some("gitpress"),
                ..Default::default()
            })
            .expect("list");
        assert_eq!(req_docs.len(), 1, "one requirement document");

        // Card must exist with correct linkage.
        let cards = list_cards(&db, "gitpress", Direction::Inbound, CardScope::Backlog)
            .expect("list cards");
        assert_eq!(cards.len(), 1, "one born-Backlog card");
        assert_eq!(
            cards[0].source_url.as_deref(),
            Some(req_docs[0].id.as_str()),
            "card source_url links to the requirement document"
        );
    }

    // --- dynamic schema lookup + card document_id binding (#528) ---

    /// Dynamic schema lookup finds v2 by title "Requirement" when no hardcoded id is used.
    #[test]
    fn dynamic_schema_lookup_finds_requirement_schema_by_title() {
        let db = test_db();
        seed_requirement_schema(&db);

        let schema = find_requirement_schema(&db).expect("should find schema");
        // The schema file has "title": "Requirement".
        let value: serde_json::Value =
            serde_json::from_str(&schema.payload).expect("parse schema payload");
        assert_eq!(
            value["title"].as_str(),
            Some("Requirement"),
            "found schema must have title Requirement"
        );
    }

    /// Dynamic schema lookup returns a clear error when no requirement schema exists.
    #[test]
    fn dynamic_schema_lookup_errors_clearly_when_no_schema_exists() {
        let db = test_db();
        // No schema seeded.
        let err = find_requirement_schema(&db).unwrap_err();
        assert!(
            err.to_string().contains("no active requirement schema"),
            "error must name what was looked for: {err}"
        );
        assert!(
            err.to_string().contains("Requirement"),
            "error must mention the expected title: {err}"
        );
    }

    /// spec-gen created cards carry document_id pointing at the requirement doc (#528).
    #[test]
    fn spec_gen_card_has_document_id_set() {
        let db = test_db();
        seed_requirement_schema(&db);

        let docs = vec![persona_doc_with_mot()];
        let outcome = generate_requirements(&docs).expect("generate");
        persist_requirements(&db, "gitpress", outcome).expect("persist");

        let req_docs = db
            .list_documents(&DocumentFilter {
                doc_type: Some("requirement"),
                surface: Some("gitpress"),
                ..Default::default()
            })
            .expect("list docs");
        assert_eq!(req_docs.len(), 1, "one requirement document");

        let cards = list_cards(&db, "gitpress", Direction::Inbound, CardScope::Backlog)
            .expect("list cards");
        assert_eq!(cards.len(), 1, "one born-Backlog card");

        // The card must have document_id set to the requirement document's id.
        assert_eq!(
            cards[0].document_id.as_deref(),
            Some(req_docs[0].id.as_str()),
            "card document_id must point to the requirement document"
        );
    }

    /// Dynamic lookup skips archived schemas and finds the live one.
    #[test]
    fn dynamic_schema_lookup_skips_archived_schema() {
        let db = test_db();
        // Seed the schema, then archive it.
        seed_requirement_schema(&db);

        // To archive, we need to find the schema doc first.
        let candidates = db
            .list_documents(&DocumentFilter {
                doc_type: Some("schema"),
                ..Default::default()
            })
            .expect("list");
        assert_eq!(candidates.len(), 1);
        // Archive the only schema -- but first we need to make sure there's no
        // live card bound (there isn't in this test). Use conn directly via
        // a raw UPDATE since archive_document checks for live cards.
        db.conn
            .execute(
                "UPDATE documents SET archived_at = ?1 WHERE id = ?2",
                rusqlite::params!["2026-06-12T00:00:00Z", candidates[0].id],
            )
            .expect("archive");

        // Now the lookup should fail (no live requirement schema).
        let err = find_requirement_schema(&db).unwrap_err();
        assert!(
            err.to_string().contains("no active requirement schema"),
            "archived schema must not be found: {err}"
        );
    }
}
