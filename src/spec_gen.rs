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
//! Deduplication key: `(traces_to, meta.surface)`.  Before creating a
//! requirement document, `persist_requirements` queries existing non-archived
//! documents of type `requirement` on the surface and skips candidates whose
//! `traces_to` already exists.

use chrono::Utc;
use serde_json::json;
use uuid::Uuid;

use crate::db::Database;
use crate::documents::{Document, DocumentFilter, DocumentMeta, validate_instance};
use crate::error::{LegionError, Result};
use crate::kanban::{self, Priority};

const REQUIREMENT_SCHEMA_ID: &str = "019eb45a-2fdd-7fd2-a574-96a3302bd15a";

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

/// A candidate that was rejected because its `traces_to` did not resolve.
#[derive(Debug, Clone)]
pub struct Rejection {
    /// The candidate title (for human-readable output).
    pub title: String,
    /// The traces_to value that failed to resolve.
    pub traces_to: String,
    /// Human-readable reason for rejection.
    pub reason: String,
}

/// Output of `generate_requirements`.
#[derive(Debug, Default)]
pub struct GenerateOutcome {
    /// Fully-validated requirement payloads, ready for persistence.
    pub requirements: Vec<Requirement>,
    /// Candidates rejected because their `traces_to` did not resolve.
    pub rejected: Vec<Rejection>,
}

/// Output of `persist_requirements`.
#[derive(Debug, Default)]
pub struct PersistOutcome {
    /// Number of new requirement documents and cards created.
    pub created: usize,
    /// Number of candidates skipped because their traces_to already existed.
    pub skipped_existing: usize,
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
/// For each `Requirement` in `outcome.requirements`:
/// 1. Check for an existing document with the same `(traces_to, surface)`.
///    If found, skip it (increment `skipped_existing`).
/// 2. Validate the payload against the requirement schema document from the DB.
///    A validation failure is a hard `Err` (bug in spec-gen).
/// 3. Insert the requirement document (`doc_type="requirement"`, `status="draft"`,
///    `owner="legion"`).
/// 4. Create a born-Backlog kanban card linked to the document.
pub fn persist_requirements(
    db: &Database,
    surface: &str,
    outcome: GenerateOutcome,
) -> Result<PersistOutcome> {
    // Load the requirement schema document for validation.
    let schema_doc = db.get_document(REQUIREMENT_SCHEMA_ID)?.ok_or_else(|| {
        LegionError::WorkSource(format!(
            "requirement schema document '{}' not found -- run `legion document create` \
                 to land the adopted schema first",
            REQUIREMENT_SCHEMA_ID
        ))
    })?;
    let schema_value: serde_json::Value =
        serde_json::from_str(&schema_doc.payload).map_err(|e| {
            LegionError::WorkSource(format!(
                "stored requirement schema payload is not valid JSON: {e}"
            ))
        })?;

    // Build the set of existing (traces_to) values on this surface to enable
    // deduplication without repeated queries.
    let existing_filter = DocumentFilter {
        doc_type: Some("requirement"),
        surface: Some(surface),
        ..Default::default()
    };
    let existing_docs = db.list_documents(&existing_filter)?;
    let existing_traces: std::collections::HashSet<String> = existing_docs
        .iter()
        .filter_map(|d| {
            serde_json::from_str::<serde_json::Value>(&d.payload)
                .ok()
                .and_then(|v| v["traces_to"].as_str().map(str::to_string))
        })
        .collect();

    let mut persist_outcome = PersistOutcome {
        rejected: outcome.rejected,
        ..Default::default()
    };

    for req in outcome.requirements {
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

        // Insert the requirement document.
        let meta = DocumentMeta {
            id: Some(&req.id),
            doc_type: "requirement",
            surface: Some(&req.surface),
            status: Some("draft"),
            priority: None,
            owner: "legion",
        };
        let doc = db.insert_document(&meta, &req.payload)?;

        // Create a born-Backlog card linked to the document.
        kanban::create_card(
            db,
            "legion",
            surface,
            &req.title,
            Some(req.description.as_str()),
            Priority::Med,
            None,
            None,
            Some(doc.id.as_str()),
            Some("document"),
            None,
        )?;

        persist_outcome.created += 1;
    }

    Ok(persist_outcome)
}

// -- private helpers ---------------------------------------------------------

/// Extract `moments_of_truth` entries from an ecosystem document and add
/// one candidate per entry.  Duplicate moment numbers within a single
/// document make the fragment ambiguous; those entries are rejected.
fn collect_ecosystem_candidates(doc: &Document, outcome: &mut GenerateOutcome) -> Result<()> {
    let surface = doc.surface.as_deref().unwrap_or("unknown");
    let payload: serde_json::Value = serde_json::from_str(&doc.payload).map_err(|e| {
        LegionError::WorkSource(format!(
            "ecosystem document '{}' has invalid JSON payload: {e}",
            doc.id
        ))
    })?;

    let moments = match payload["moments_of_truth"].as_array() {
        Some(arr) => arr,
        None => return Ok(()), // no moments: no candidates
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
        let title_raw = entry["title"].as_str().unwrap_or("");
        let actor = entry["actor"].as_str().unwrap_or("");
        let success = entry["success"].as_str().unwrap_or("");
        let failure = entry["failure"].as_str().unwrap_or("");

        let candidate_title = format!("MoT: {title_raw}");

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

        let req = build_requirement(
            doc.id.as_str(),
            surface,
            &candidate_title,
            &description,
            &traces_to,
        )?;
        outcome.requirements.push(req);
    }

    Ok(())
}

/// Extract the `moment_of_truth` singleton from a persona document, if present.
fn collect_persona_candidates(doc: &Document, outcome: &mut GenerateOutcome) -> Result<()> {
    let surface = doc.surface.as_deref().unwrap_or("unknown");
    let payload: serde_json::Value = serde_json::from_str(&doc.payload).map_err(|e| {
        LegionError::WorkSource(format!(
            "persona document '{}' has invalid JSON payload: {e}",
            doc.id
        ))
    })?;

    // Persona singleton moment_of_truth is optional; no field means no candidate.
    let mot = match payload.get("moment_of_truth") {
        Some(v) if !v.is_null() => v,
        _ => return Ok(()),
    };

    let description_text = mot["description"].as_str().unwrap_or("");
    let success = mot["success"].as_str().unwrap_or("");
    let failure = mot["failure"].as_str().unwrap_or("");

    // Use the persona's meta.title for the candidate title, falling back to doc id.
    let persona_title = payload["meta"]["title"].as_str().unwrap_or(doc.id.as_str());
    let candidate_title = format!("MoT: {persona_title}");
    let traces_to = format!("{}#moment_of_truth", doc.id);

    let description = if success.is_empty() && failure.is_empty() {
        description_text.to_string()
    } else {
        format!("{description_text} Success: {success}. Failure: {failure}.")
    };

    let req = build_requirement(
        doc.id.as_str(),
        surface,
        &candidate_title,
        &description,
        &traces_to,
    )?;
    outcome.requirements.push(req);

    Ok(())
}

/// Build a fully-composed `Requirement` struct with a valid JSON payload.
fn build_requirement(
    _source_doc_id: &str,
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

    /// Land the requirement schema doc so persist_requirements can validate.
    fn seed_requirement_schema(db: &Database) {
        let schema_payload = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/schemas/requirement.schema.json"
        ))
        .expect("requirement schema file");
        let meta = DocumentMeta {
            id: Some(REQUIREMENT_SCHEMA_ID),
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
        // This test verifies the validation path by directly calling
        // validate_instance with a deliberately malformed payload that is
        // missing required fields.
        let schema_payload = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/schemas/requirement.schema.json"
        ))
        .expect("requirement schema file");
        let schema: serde_json::Value =
            serde_json::from_str(&schema_payload).expect("parse schema");

        // A payload missing all required fields.
        let bad_payload = serde_json::json!({});
        let errors = validate_instance(&schema, &bad_payload);
        assert!(
            !errors.is_empty(),
            "malformed payload must have validation errors"
        );
        // Should complain about missing 'meta', 'title', 'description', 'traces_to'.
        let joined = errors.join("; ");
        assert!(
            joined.contains("meta"),
            "must report missing meta: {joined}"
        );
    }
}
