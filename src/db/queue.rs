//! Queue storage (#934): the pick-next-work selection and the
//! done-transition, extracted into a seam that reads only work-item
//! identity and scheduling fields (id, to_repo, priority, status,
//! sort_order, created_at) -- never card content (text, context, problem,
//! solution, acceptance, labels, source_url). Card content is duplicated
//! from the issue and is scheduled to DROP (#934's classification); the
//! scheduling behavior itself is legion-specific and worth keeping, so it
//! moves to a home that does not need the content fields to work.
//!
//! Storage does NOT move: this still reads and writes the `tasks` table
//! (defined in `db/kanban.rs`). That table is what #931 repoints once the
//! card surface goes -- this module is deliberately independent of
//! `db/kanban.rs`'s own types (`CardStatus`, `Action`, `Card`) so it does
//! not carry a dependency on the surface being removed. Status and
//! priority are read and written as plain strings, matching how the
//! `tasks` table itself stores them.
//!
//! `PRIORITY_ORDER` intentionally duplicates `Database::PRIORITY_ORDER`
//! (db/kanban.rs) rather than importing it: the two modules must not
//! depend on each other, so `priority_order_sql_covers_every_priority_variant`
//! below independently pins this copy against `kanban::Priority` the same
//! way the card-surface version pins its own.

use rusqlite::OptionalExtension;

use super::Database;
use crate::error::{LegionError, Result};
use crate::queue::QueueItem;

const QUEUE_COLUMNS: &str = "id, to_repo, priority, status, sort_order, created_at";

/// SQL fragment for priority ordering. Deliberately a standalone copy of
/// `Database::PRIORITY_ORDER` (db/kanban.rs) -- see the module doc comment.
const PRIORITY_ORDER: &str = "CASE priority WHEN 'critical' THEN 0 WHEN 'high' THEN 1 \
     WHEN 'med' THEN 2 WHEN 'low' THEN 3 END";

/// Statuses from which `complete_queue_work` accepts a transition to
/// `done`. Mirrors the two arms of `kanban::state::transition` that reach
/// `CardStatus::Done` (`Accepted -> Done`, `InReview -> Done`) expressed as
/// plain status strings instead of the `CardStatus`/`Action` enums, so this
/// module carries no dependency on the card-surface state machine. A
/// status outside this set is refused, not silently accepted -- losing
/// this guard would be a regression, not a move.
const DONE_FROM_STATUSES: [&str; 2] = ["accepted", "in-review"];

fn map_queue_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<QueueItem> {
    Ok(QueueItem {
        id: row.get(0)?,
        to_repo: row.get(1)?,
        priority: row.get(2)?,
        status: row.get(3)?,
        sort_order: row.get::<_, Option<i32>>(4)?.unwrap_or(0),
        created_at: row.get(5)?,
    })
}

impl Database {
    /// Ids of every `status = 'accepted'` work item for `repo`, ordered the
    /// same way inbound cards are ordered for the board (priority, then
    /// sort_order, then newest-first) -- matches
    /// `kanban::list_cards(Direction::Inbound, CardScope::WorkingSet)`
    /// narrowed to `Accepted` by `kanban::format_active_goal`, expressed
    /// directly as a status-literal query so this seam needs neither the
    /// `Direction` nor `CardScope` enums.
    ///
    /// Backs `legion goal` (#934): *which* cards are the active goal is
    /// legion-specific scheduling state, so it lives here; *what the card
    /// says* stays a separate content lookup at the caller
    /// (`cli::misc::handle_goal` reads each id back via `get_card_by_id`
    /// for `kanban::format_active_goal`), the same identity-then-content
    /// split `queue::next_work` / `handle_work` already uses.
    pub fn accepted_work_item_ids(&self, repo: &str) -> Result<Vec<String>> {
        let sql = format!(
            "SELECT id FROM tasks WHERE to_repo = ?1 AND status = 'accepted' \
             AND deleted_at IS NULL ORDER BY {PRIORITY_ORDER}, sort_order ASC, created_at DESC"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params![repo], |row| row.get::<_, String>(0))?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(LegionError::Database)
    }

    /// Peek at the next pending work item for a repo without claiming it.
    ///
    /// Selects highest priority, then lowest sort_order, then oldest --
    /// identical ordering to the card-surface predecessor
    /// (`pick_next_card`/`peek_next_card`, db/kanban.rs), since the
    /// ordering behavior is exactly what #934 classifies as worth keeping.
    pub fn peek_next_pending_work(&self, repo: &str) -> Result<Option<QueueItem>> {
        let sql = format!(
            "SELECT {QUEUE_COLUMNS} FROM tasks WHERE to_repo = ?1 AND status = 'pending' \
             AND deleted_at IS NULL ORDER BY {PRIORITY_ORDER}, sort_order ASC, created_at ASC LIMIT 1"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let row = stmt
            .query_row(rusqlite::params![repo], map_queue_row)
            .optional()
            .map_err(LegionError::Database)?;
        Ok(row)
    }

    /// Atomically pick the next pending work item for a repo and mark it
    /// accepted.
    ///
    /// The `AND status = 'pending'` predicate on the write makes the claim
    /// conditional: a second concurrent picker's write affects zero rows,
    /// which reads back as `Ok(None)` here -- the same race-safety
    /// `pick_next_card` provides for cards, expressed without depending on
    /// its `CardTimestamp`/`CardStatus` types.
    pub fn pick_next_pending_work(&self, repo: &str) -> Result<Option<QueueItem>> {
        let Some(candidate) = self.peek_next_pending_work(repo)? else {
            return Ok(None);
        };

        let now = chrono::Utc::now().to_rfc3339();
        let rows = self.conn.execute(
            "UPDATE tasks SET status = 'accepted', started_at = ?1, updated_at = ?1 \
             WHERE id = ?2 AND status = 'pending' AND deleted_at IS NULL",
            rusqlite::params![&now, &candidate.id],
        )?;
        if rows == 0 {
            // Lost the race to another picker between the peek and the
            // write -- not an error, just no work claimed this call.
            return Ok(None);
        }

        let mut claimed = candidate;
        claimed.status = "accepted".to_string();
        Ok(Some(claimed))
    }

    /// Transition a work item to `done`, guarded to only accept the
    /// transition from `accepted` or `in-review` (see `DONE_FROM_STATUSES`)
    /// -- the same legality `kanban::transition_card(Action::Done)`
    /// enforces via its FSM table, preserved here as a plain status guard
    /// so this module needs no dependency on that table.
    ///
    /// Returns `CardNotFound` when the id does not exist, or
    /// `InvalidCardTransition` when it exists but is not in an
    /// allowed-from status.
    pub fn complete_queue_work(&self, id: &str, note: Option<&str>) -> Result<()> {
        let now = chrono::Utc::now().to_rfc3339();
        let placeholders = DONE_FROM_STATUSES
            .iter()
            .map(|_| "?")
            .collect::<Vec<_>>()
            .join(", ");
        let sql = format!(
            "UPDATE tasks SET status = 'done', note = COALESCE(?1, note), \
             completed_at = ?2, updated_at = ?2 \
             WHERE id = ?3 AND status IN ({placeholders}) AND deleted_at IS NULL"
        );
        let mut params: Vec<&dyn rusqlite::types::ToSql> = vec![&note, &now, &id];
        for status in DONE_FROM_STATUSES.iter() {
            params.push(status);
        }
        let rows = self.conn.execute(&sql, params.as_slice())?;
        if rows == 1 {
            return Ok(());
        }

        // Distinguish "no such row" from "row exists but not in an
        // allowed-from status" so the caller gets an actionable error
        // rather than a generic not-found for both.
        let current: Option<String> = self
            .conn
            .query_row(
                "SELECT status FROM tasks WHERE id = ?1 AND deleted_at IS NULL",
                rusqlite::params![id],
                |row| row.get(0),
            )
            .optional()
            .map_err(LegionError::Database)?;
        match current {
            None => Err(LegionError::CardNotFound(id.to_string())),
            Some(status) => Err(LegionError::InvalidCardTransition {
                action: "done".to_string(),
                current: status,
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::testutil::test_db;

    /// Mirrors `priority_order_sql_covers_every_priority_variant`
    /// (db/kanban.rs) -- pins this module's independent copy of the
    /// priority CASE expression against the same enum, so a new
    /// `kanban::Priority` variant forces a decision here too, not just in
    /// the card surface.
    #[test]
    fn priority_order_sql_covers_every_priority_variant() {
        use clap::ValueEnum;
        for p in crate::kanban::Priority::value_variants() {
            let arm = format!("WHEN '{p}' THEN");
            assert!(
                PRIORITY_ORDER.contains(&arm),
                "PRIORITY_ORDER is missing an arm for priority '{p}'"
            );
        }
    }

    fn insert_pending(db: &Database, repo: &str, priority: crate::kanban::Priority) -> String {
        db.insert_card(
            "kelex",
            repo,
            "queue test item",
            None,
            priority,
            None,
            None,
            None,
            None,
            None,
            crate::kanban::CardStatus::Pending,
        )
        .expect("insert pending")
    }

    #[test]
    fn peek_next_pending_work_does_not_claim() {
        let db = test_db();
        let id = insert_pending(&db, "kelex", crate::kanban::Priority::Med);

        let peeked = db
            .peek_next_pending_work("kelex")
            .unwrap()
            .expect("candidate");
        assert_eq!(peeked.id, id);
        assert_eq!(peeked.status, "pending");

        // A second peek must still see it pending -- peek never claims.
        let peeked_again = db
            .peek_next_pending_work("kelex")
            .unwrap()
            .expect("still there");
        assert_eq!(peeked_again.status, "pending");
    }

    #[test]
    fn pick_next_pending_work_claims_and_transitions_to_accepted() {
        let db = test_db();
        let id = insert_pending(&db, "kelex", crate::kanban::Priority::Med);

        let picked = db
            .pick_next_pending_work("kelex")
            .unwrap()
            .expect("candidate");
        assert_eq!(picked.id, id);
        assert_eq!(picked.status, "accepted");

        // A second pick must find nothing left pending.
        assert!(db.pick_next_pending_work("kelex").unwrap().is_none());
    }

    #[test]
    fn pick_next_pending_work_prefers_higher_priority_then_older() {
        let db = test_db();
        insert_pending(&db, "kelex", crate::kanban::Priority::Low);
        let high_id = insert_pending(&db, "kelex", crate::kanban::Priority::High);

        let picked = db
            .pick_next_pending_work("kelex")
            .unwrap()
            .expect("candidate");
        assert_eq!(picked.id, high_id, "high priority must be picked first");
    }

    #[test]
    fn pick_next_pending_work_returns_none_when_empty() {
        let db = test_db();
        assert!(db.pick_next_pending_work("kelex").unwrap().is_none());
    }

    #[test]
    fn complete_queue_work_succeeds_from_accepted() {
        let db = test_db();
        let id = insert_pending(&db, "kelex", crate::kanban::Priority::Med);
        db.pick_next_pending_work("kelex").unwrap();

        db.complete_queue_work(&id, Some("shipped"))
            .expect("complete from accepted");

        let card = db.get_card_by_id(&id).unwrap().expect("exists");
        assert_eq!(card.status.to_string(), "done");
        assert_eq!(card.note.as_deref(), Some("shipped"));
    }

    #[test]
    fn complete_queue_work_refuses_from_pending() {
        let db = test_db();
        let id = insert_pending(&db, "kelex", crate::kanban::Priority::Med);

        let err = db
            .complete_queue_work(&id, None)
            .expect_err("done from pending must be refused");
        match err {
            LegionError::InvalidCardTransition { action, current } => {
                assert_eq!(action, "done");
                assert_eq!(current, "pending");
            }
            other => panic!("expected InvalidCardTransition, got {other:?}"),
        }

        // The row must be untouched -- still pending, not done.
        let card = db.get_card_by_id(&id).unwrap().expect("exists");
        assert_eq!(card.status.to_string(), "pending");
    }

    #[test]
    fn complete_queue_work_succeeds_from_in_review() {
        let db = test_db();
        let id = insert_pending(&db, "kelex", crate::kanban::Priority::Med);
        db.pick_next_pending_work("kelex").unwrap();
        db.force_move_card(&id, "in-review", None).unwrap();

        db.complete_queue_work(&id, None)
            .expect("complete from in-review");
        let card = db.get_card_by_id(&id).unwrap().expect("exists");
        assert_eq!(card.status.to_string(), "done");
    }

    #[test]
    fn accepted_work_item_ids_returns_only_accepted_rows_for_repo() {
        let db = test_db();
        let accepted_id = insert_pending(&db, "kelex", crate::kanban::Priority::Med);
        db.force_move_card(&accepted_id, "accepted", None).unwrap();
        let pending_id = insert_pending(&db, "kelex", crate::kanban::Priority::Med);
        let other_repo_id = insert_pending(&db, "other", crate::kanban::Priority::Med);
        db.force_move_card(&other_repo_id, "accepted", None)
            .unwrap();

        let ids = db.accepted_work_item_ids("kelex").unwrap();
        assert_eq!(ids, vec![accepted_id]);
        assert!(!ids.contains(&pending_id));
    }

    #[test]
    fn accepted_work_item_ids_orders_by_priority_then_newest_first() {
        let db = test_db();
        let low_id = insert_pending(&db, "kelex", crate::kanban::Priority::Low);
        db.force_move_card(&low_id, "accepted", None).unwrap();
        let high_id = insert_pending(&db, "kelex", crate::kanban::Priority::High);
        db.force_move_card(&high_id, "accepted", None).unwrap();

        let ids = db.accepted_work_item_ids("kelex").unwrap();
        assert_eq!(ids, vec![high_id, low_id], "high priority sorts first");
    }

    #[test]
    fn accepted_work_item_ids_empty_when_none_accepted() {
        let db = test_db();
        insert_pending(&db, "kelex", crate::kanban::Priority::Med);
        assert!(db.accepted_work_item_ids("kelex").unwrap().is_empty());
    }

    #[test]
    fn complete_queue_work_errors_on_missing_id() {
        let db = test_db();
        let err = db
            .complete_queue_work("no-such-id", None)
            .expect_err("missing id must error");
        assert!(matches!(err, LegionError::CardNotFound(_)));
    }
}
