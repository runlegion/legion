//! Spec-revision protocol (#554): `ReplanRecord` storage. Owns the
//! `replan_records` DDL.
//!
//! A `ReplanRecord` is the ratification act made durable: what changed on a
//! card's frozen acceptance criteria, why, and that it was ratified before
//! work resumed (docs/decisions/2026-05-31-spec-revision-protocol.md). The
//! verify gate (`src/verify.rs`) consults the most recent record for a card
//! to tell a sanctioned re-plan apart from an unratified deviation.

use chrono::Utc;
use rusqlite::Connection;
use uuid::Uuid;

use super::Database;
use crate::error::Result;

/// `replan_records` table (#554).
pub(super) fn create_tables(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS replan_records (
                id TEXT PRIMARY KEY,
                card_id TEXT NOT NULL,
                reason TEXT NOT NULL,
                revised_at TEXT NOT NULL,
                ratified INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_replan_records_card ON replan_records(card_id);",
    )?;
    Ok(())
}

/// A recorded spec re-plan bound to a card: `{card_id, reason, revised_at,
/// ratified}` per the RFC (docs/decisions/2026-05-31-spec-revision-protocol.md).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ReplanRecord {
    pub id: String,
    pub card_id: String,
    pub reason: String,
    pub revised_at: String,
    pub ratified: bool,
}

impl Database {
    /// Record a re-plan for `card_id`. Recording one at all is normally the
    /// ratification act itself, but `ratified` is stored explicitly so a
    /// caller can distinguish a proposed-but-not-yet-ratified entry if that
    /// ever becomes a real workflow state.
    pub fn record_replan_record(
        &self,
        card_id: &str,
        reason: &str,
        ratified: bool,
    ) -> Result<ReplanRecord> {
        let id = Uuid::now_v7().to_string();
        let revised_at = Utc::now().to_rfc3339();
        self.conn.execute(
            "INSERT INTO replan_records (id, card_id, reason, revised_at, ratified)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![id, card_id, reason, revised_at, ratified],
        )?;
        Ok(ReplanRecord {
            id,
            card_id: card_id.to_owned(),
            reason: reason.to_owned(),
            revised_at,
            ratified,
        })
    }

    /// Return the most recent `ReplanRecord` for `card_id`, if any.
    ///
    /// `legion verify`'s deviation check treats `None` (or a record whose
    /// `ratified` is `false`) as "no ratified re-plan exists" -- see
    /// `verify::replan_gate`.
    pub fn get_latest_replan_record(&self, card_id: &str) -> Result<Option<ReplanRecord>> {
        let mut stmt = self.conn.prepare(
            // `ORDER BY id DESC` relies on `id` being a UUIDv7 (time-ordered
            // lexicographically): the newest record has the largest id.
            "SELECT id, card_id, reason, revised_at, ratified
             FROM replan_records
             WHERE card_id = ?1
             ORDER BY id DESC
             LIMIT 1",
        )?;
        let mut rows = stmt.query_map([card_id], |row| {
            Ok(ReplanRecord {
                id: row.get(0)?,
                card_id: row.get(1)?,
                reason: row.get(2)?,
                revised_at: row.get(3)?,
                ratified: row.get(4)?,
            })
        })?;
        match rows.next() {
            Some(row) => Ok(Some(row?)),
            None => Ok(None),
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::db::testutil::test_db;

    #[test]
    fn record_and_query_round_trip() {
        let db = test_db();
        let record = db
            .record_replan_record("card-1", "step 2 revealed the AC were wrong", true)
            .expect("record");
        assert!(!record.id.is_empty());
        assert_eq!(record.card_id, "card-1");
        assert!(record.ratified);

        let fetched = db
            .get_latest_replan_record("card-1")
            .expect("query")
            .expect("some record");
        assert_eq!(fetched, record);
    }

    #[test]
    fn no_record_returns_none() {
        let db = test_db();
        assert!(
            db.get_latest_replan_record("no-such-card")
                .expect("query")
                .is_none()
        );
    }

    #[test]
    fn latest_record_wins_when_multiple_exist() {
        let db = test_db();
        db.record_replan_record("card-1", "first re-plan", true)
            .expect("first record");
        let second = db
            .record_replan_record("card-1", "second re-plan", true)
            .expect("second record");

        let fetched = db
            .get_latest_replan_record("card-1")
            .expect("query")
            .expect("some record");
        assert_eq!(fetched.id, second.id);
        assert_eq!(fetched.reason, "second re-plan");
    }

    #[test]
    fn unratified_record_is_stored_as_such() {
        let db = test_db();
        let record = db
            .record_replan_record("card-1", "proposed but not yet ratified", false)
            .expect("record");
        assert!(!record.ratified);

        let fetched = db
            .get_latest_replan_record("card-1")
            .expect("query")
            .expect("some record");
        assert!(!fetched.ratified);
    }
}
