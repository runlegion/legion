//! Deferral storage (#934): a card-independent home for "put this work off
//! until a future time." Mirrors `db/autonomy.rs` and `db/wake.rs`'s shape
//! (a small dedicated table, no FSM) rather than living on the `tasks`
//! table the way the card-scoped predecessor (`tasks.wake_at` /
//! `tasks.pre_defer_status`, #816) does.
//!
//! Keyed on an opaque `work_item_id`, not a card row: nothing here reads or
//! writes `tasks`. Unlike the card version, there is no `pre_defer_status`
//! -- a card needs a revert target because it has a status to go back to;
//! a bare deferral has no such state machine, so on wake it simply stops
//! existing and its owner gets a signal. Porting `pre_defer_status` would
//! re-import the card dependency this table exists to remove.

use chrono::Utc;
use rusqlite::Connection;

use super::Database;
use crate::error::Result;

/// `deferrals` table (#934).
pub(super) fn create_tables(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS deferrals (
                work_item_id TEXT PRIMARY KEY,
                repo TEXT NOT NULL,
                wake_at TEXT NOT NULL,
                note TEXT,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_deferrals_wake_at ON deferrals(wake_at);",
    )?;
    Ok(())
}

impl Database {
    /// Defer `work_item_id`, owned by `repo`, until `wake_at` (already
    /// validated as a future RFC3339 timestamp by the caller -- mirrors
    /// `kanban::defer_card`'s contract, where the CLI layer parses `--until`
    /// and refuses a past time before this is ever reached).
    ///
    /// Upsert, not insert-only: re-deferring the same work item updates
    /// `wake_at`/`note` in place rather than erroring, matching the card
    /// version's re-defer-from-Deferred legality.
    pub fn upsert_deferral(
        &self,
        work_item_id: &str,
        repo: &str,
        wake_at: &str,
        note: Option<&str>,
    ) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        self.conn.execute(
            "INSERT INTO deferrals (work_item_id, repo, wake_at, note, created_at, updated_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?5) \
             ON CONFLICT(work_item_id) DO UPDATE SET \
                repo = excluded.repo, \
                wake_at = excluded.wake_at, \
                note = excluded.note, \
                updated_at = excluded.updated_at",
            rusqlite::params![work_item_id, repo, wake_at, note, &now],
        )?;
        Ok(())
    }

    /// Look up the deferral for a work item, if one is active.
    pub fn get_deferral(&self, work_item_id: &str) -> Result<Option<crate::defer::Deferral>> {
        let mut stmt = self.conn.prepare(
            "SELECT work_item_id, repo, wake_at, note, created_at, updated_at \
             FROM deferrals WHERE work_item_id = ?1",
        )?;
        let mut rows = stmt.query_map(rusqlite::params![work_item_id], map_deferral_row)?;
        match rows.next() {
            Some(row) => Ok(Some(row?)),
            None => Ok(None),
        }
    }

    /// Clear a work item's deferral. Returns `true` when a row existed and
    /// was removed, `false` for a no-op on a work item that was not
    /// deferred -- mirroring `soft_delete_card`'s boolean-return contract
    /// rather than erroring on an already-cleared deferral.
    pub fn clear_deferral(&self, work_item_id: &str) -> Result<bool> {
        let removed = self.conn.execute(
            "DELETE FROM deferrals WHERE work_item_id = ?1",
            rusqlite::params![work_item_id],
        )?;
        Ok(removed > 0)
    }

    /// Every deferral whose `wake_at` has passed (#934): the scheduled-wake
    /// sweep target for `tick_health`, mirroring
    /// `get_deferred_cards_due`'s card-scoped predecessor but reading
    /// `deferrals` instead of `tasks`.
    pub fn deferrals_due(&self, now: &str) -> Result<Vec<crate::defer::Deferral>> {
        let mut stmt = self.conn.prepare(
            "SELECT work_item_id, repo, wake_at, note, created_at, updated_at \
             FROM deferrals WHERE wake_at <= ?1 ORDER BY wake_at ASC",
        )?;
        let rows = stmt.query_map(rusqlite::params![now], map_deferral_row)?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(crate::error::LegionError::Database)
    }
}

fn map_deferral_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<crate::defer::Deferral> {
    Ok(crate::defer::Deferral {
        work_item_id: row.get(0)?,
        repo: row.get(1)?,
        wake_at: row.get(2)?,
        note: row.get(3)?,
        created_at: row.get(4)?,
        updated_at: row.get(5)?,
    })
}

#[cfg(test)]
mod tests {
    use crate::db::testutil::test_db;

    #[test]
    fn upsert_and_get_deferral_round_trips() {
        let db = test_db();
        assert!(db.get_deferral("item-1").unwrap().is_none());

        db.upsert_deferral(
            "item-1",
            "legion",
            "2099-01-01T00:00:00+00:00",
            Some("note"),
        )
        .unwrap();
        let got = db.get_deferral("item-1").unwrap().expect("exists");
        assert_eq!(got.work_item_id, "item-1");
        assert_eq!(got.repo, "legion");
        assert_eq!(got.wake_at, "2099-01-01T00:00:00+00:00");
        assert_eq!(got.note.as_deref(), Some("note"));
    }

    #[test]
    fn upsert_deferral_is_a_re_defer_not_a_duplicate() {
        let db = test_db();
        db.upsert_deferral("item-1", "legion", "2099-01-01T00:00:00+00:00", None)
            .unwrap();
        db.upsert_deferral(
            "item-1",
            "legion",
            "2099-06-01T00:00:00+00:00",
            Some("updated"),
        )
        .unwrap();

        let got = db.get_deferral("item-1").unwrap().expect("exists");
        assert_eq!(got.wake_at, "2099-06-01T00:00:00+00:00");
        assert_eq!(got.note.as_deref(), Some("updated"));

        let due = db.deferrals_due("2100-01-01T00:00:00+00:00").unwrap();
        assert_eq!(due.len(), 1, "re-defer must not leave a duplicate row");
    }

    #[test]
    fn clear_deferral_removes_row_and_reports_whether_one_existed() {
        let db = test_db();
        db.upsert_deferral("item-1", "legion", "2099-01-01T00:00:00+00:00", None)
            .unwrap();

        assert!(db.clear_deferral("item-1").unwrap());
        assert!(db.get_deferral("item-1").unwrap().is_none());
        assert!(
            !db.clear_deferral("item-1").unwrap(),
            "clearing an already-cleared deferral is a no-op, not an error"
        );
    }

    #[test]
    fn deferrals_due_only_returns_past_wake_at() {
        let db = test_db();
        db.upsert_deferral("due", "legion", "2020-01-01T00:00:00+00:00", None)
            .unwrap();
        db.upsert_deferral("not-due", "legion", "2099-01-01T00:00:00+00:00", None)
            .unwrap();

        let now = chrono::Utc::now().to_rfc3339();
        let due = db.deferrals_due(&now).unwrap();
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].work_item_id, "due");
    }

    #[test]
    fn deferrals_isolated_by_work_item_id() {
        let db = test_db();
        db.upsert_deferral("item-a", "legion", "2099-01-01T00:00:00+00:00", None)
            .unwrap();
        assert!(db.get_deferral("item-b").unwrap().is_none());
    }
}
