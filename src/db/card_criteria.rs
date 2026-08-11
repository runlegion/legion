//! `card_criteria` join table (#882 step 1): which spec criteria a card
//! declares it is servicing.
//!
//! Additive metadata alongside the existing whole-document `tasks.document_id`
//! binding (`bind_card_to_document`) -- it does not replace it. A card must
//! already be bound to a document before it can declare serviced criteria,
//! and every declared id must exist in that document's current
//! `verification.criteria` set (checked against the payload directly, not
//! a separate projection table -- see the #882 step 1 design doc for why a
//! `document_criteria` projection table was scoped out).

use chrono::Utc;
use rusqlite::{Connection, params};

use super::Database;
use crate::error::{LegionError, Result};

/// `card_criteria` table (#882 step 1).
pub(super) fn create_tables(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS card_criteria (
            card_id TEXT NOT NULL,
            criterion_id TEXT NOT NULL,
            created_at TEXT NOT NULL,
            PRIMARY KEY (card_id, criterion_id)
        );
        CREATE INDEX IF NOT EXISTS idx_card_criteria_card ON card_criteria(card_id);",
    )?;
    Ok(())
}

/// Read the set of criterion ids present in a document's payload
/// (`verification.criteria[].id`). Returns an empty set for a payload with
/// no such array, a corrupt payload, or a document still on the legacy
/// id-less `verification.acceptance` shape.
fn valid_criterion_ids(payload: &str) -> std::collections::HashSet<String> {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(payload) else {
        return std::collections::HashSet::new();
    };
    let Some(arr) = value
        .get("verification")
        .and_then(|v| v.get("criteria"))
        .and_then(|c| c.as_array())
    else {
        return std::collections::HashSet::new();
    };
    arr.iter()
        .filter_map(|v| v.get("id").and_then(|i| i.as_str()))
        .map(str::to_string)
        .collect()
}

impl Database {
    /// Replace the set of spec criteria a card declares it is servicing
    /// (#882 step 1). Idempotent replace, not append: a re-run with a
    /// shorter list drops the criteria left out.
    ///
    /// Errors when:
    /// - The card does not exist.
    /// - The card has no bound document (`tasks.document_id` is `None`) --
    ///   bind it first via `legion kanban bind`.
    /// - The bound document does not exist (dangling reference).
    /// - The bound document has no id-carrying `verification.criteria`
    ///   (still on the legacy string-array shape, or has none at all).
    /// - Any requested id is not a member of that document's current
    ///   criteria set -- same posture as verify's dangling-document_id hard
    ///   error: a card cannot claim to service spec state that does not
    ///   exist.
    pub fn set_card_criteria(&self, card_id: &str, criterion_ids: &[String]) -> Result<()> {
        let card = self
            .get_card_by_id(card_id)?
            .ok_or_else(|| LegionError::CardNotFound(card_id.to_string()))?;
        let doc_id = card.document_id.ok_or_else(|| {
            LegionError::WorkSource(format!(
                "card '{card_id}' has no bound document -- bind it first \
                 (`legion kanban bind --id {card_id} --document <doc>`) before \
                 declaring serviced criteria"
            ))
        })?;
        let doc = self.get_document(&doc_id)?.ok_or_else(|| {
            LegionError::WorkSource(format!(
                "card '{card_id}' is bound to document '{doc_id}' but it does not exist"
            ))
        })?;
        let valid = valid_criterion_ids(&doc.payload);
        if valid.is_empty() {
            return Err(LegionError::WorkSource(format!(
                "document '{doc_id}' has no id-carrying verification.criteria to service \
                 (it may still be on the legacy verification.acceptance shape)"
            )));
        }
        for id in criterion_ids {
            if !valid.contains(id) {
                return Err(LegionError::WorkSource(format!(
                    "criterion id '{id}' does not exist in document '{doc_id}'s current criteria"
                )));
            }
        }

        let now = Utc::now().to_rfc3339();
        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "DELETE FROM card_criteria WHERE card_id = ?1",
            params![card_id],
        )?;
        for id in criterion_ids {
            tx.execute(
                "INSERT INTO card_criteria (card_id, criterion_id, created_at) VALUES (?1, ?2, ?3)",
                params![card_id, id, &now],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// List the criterion ids a card currently declares as serviced,
    /// insertion order (i.e. `set_card_criteria`'s call order).
    pub fn card_criteria(&self, card_id: &str) -> Result<Vec<String>> {
        let mut stmt = self.conn.prepare(
            "SELECT criterion_id FROM card_criteria WHERE card_id = ?1 ORDER BY created_at",
        )?;
        let rows = stmt.query_map(params![card_id], |row| row.get(0))?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::testutil::test_db;
    use crate::documents::DocumentMeta;
    use crate::kanban::{CardStatus, Priority};

    fn seed_doc_with_criteria(db: &Database, id: &str) -> Vec<String> {
        let meta = DocumentMeta {
            id: Some(id),
            doc_type: "requirement",
            surface: None,
            status: None,
            priority: None,
            owner: "legion",
        };
        let payload = serde_json::json!({
            "verification": {
                "criteria": [
                    {"text": "first"},
                    {"text": "second"}
                ]
            }
        })
        .to_string();
        let doc = db.insert_document(&meta, &payload).expect("insert doc");
        let value: serde_json::Value = serde_json::from_str(&doc.payload).unwrap();
        value["verification"]["criteria"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c["id"].as_str().unwrap().to_string())
            .collect()
    }

    fn seed_bound_card(db: &Database, doc_id: &str) -> String {
        let card_id = db
            .insert_card(
                "legion",
                "legion",
                "test card",
                None,
                Priority::Med,
                None,
                None,
                None,
                None,
                None,
                CardStatus::Accepted,
            )
            .expect("insert card");
        db.bind_card_to_document(&card_id, doc_id).expect("bind");
        card_id
    }

    #[test]
    fn set_and_read_card_criteria_round_trips() {
        let db = test_db();
        let ids = seed_doc_with_criteria(&db, "doc-crit-1");
        let card_id = seed_bound_card(&db, "doc-crit-1");

        db.set_card_criteria(&card_id, &ids).expect("declare");
        let read = db.card_criteria(&card_id).expect("read");
        assert_eq!(read, ids);
    }

    #[test]
    fn set_card_criteria_is_a_replace_not_an_append() {
        let db = test_db();
        let ids = seed_doc_with_criteria(&db, "doc-crit-2");
        let card_id = seed_bound_card(&db, "doc-crit-2");

        db.set_card_criteria(&card_id, &ids).expect("declare both");
        assert_eq!(db.card_criteria(&card_id).unwrap().len(), 2);

        db.set_card_criteria(&card_id, &ids[..1])
            .expect("declare one");
        assert_eq!(db.card_criteria(&card_id).unwrap(), vec![ids[0].clone()]);
    }

    #[test]
    fn set_card_criteria_rejects_unknown_id() {
        let db = test_db();
        seed_doc_with_criteria(&db, "doc-crit-3");
        let card_id = seed_bound_card(&db, "doc-crit-3");

        let err = db
            .set_card_criteria(&card_id, &["bogus-id".to_string()])
            .unwrap_err();
        assert!(err.to_string().contains("does not exist"), "got: {err}");
    }

    #[test]
    fn set_card_criteria_rejects_unbound_card() {
        let db = test_db();
        let card_id = db
            .insert_card(
                "legion",
                "legion",
                "unbound card",
                None,
                Priority::Med,
                None,
                None,
                None,
                None,
                None,
                CardStatus::Accepted,
            )
            .expect("insert card");

        let err = db
            .set_card_criteria(&card_id, &["whatever".to_string()])
            .unwrap_err();
        assert!(err.to_string().contains("no bound document"), "got: {err}");
    }

    #[test]
    fn set_card_criteria_rejects_legacy_acceptance_only_document() {
        let db = test_db();
        let meta = DocumentMeta {
            id: Some("doc-legacy-crit"),
            doc_type: "requirement",
            surface: None,
            status: None,
            priority: None,
            owner: "legion",
        };
        db.insert_document(
            &meta,
            &serde_json::json!({"verification": {"acceptance": ["plain string"]}}).to_string(),
        )
        .expect("insert");
        let card_id = seed_bound_card(&db, "doc-legacy-crit");

        let err = db
            .set_card_criteria(&card_id, &["whatever".to_string()])
            .unwrap_err();
        assert!(err.to_string().contains("no id-carrying"), "got: {err}");
    }

    #[test]
    fn card_criteria_empty_for_card_with_no_declaration() {
        let db = test_db();
        let ids = seed_doc_with_criteria(&db, "doc-crit-4");
        let card_id = seed_bound_card(&db, "doc-crit-4");
        let _ = &ids;
        assert!(db.card_criteria(&card_id).unwrap().is_empty());
    }
}
