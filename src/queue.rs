//! Queue: `legion work`'s pick-next selection and `legion done`'s
//! done-transition, as a behavior seam independent of the card surface
//! (#934). Storage does not move -- this still reads and writes the
//! `tasks` table via `src/db/queue.rs` -- but the SELECTION and
//! TRANSITION logic no longer needs the `Card`/`CardStatus`/`Action`
//! types `src/kanban/mod.rs` owns, so it survives that module's eventual
//! removal (#931).
//!
//! `QueueItem` deliberately carries only identity and scheduling fields
//! (id, repo, priority, status, sort_order, created_at) -- never card
//! content (text, context, problem, solution, acceptance, labels,
//! source_url). Content display for `legion work` is a separate lookup at
//! the CLI layer (`cli::misc::handle_work` reads the card by id for
//! `kanban::format_work_card`'s existing output), not something this seam
//! needs to know about to pick or complete work correctly.

use crate::db::Database;
use crate::error::Result;

/// A work item as the queue sees it: identity and scheduling fields only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueueItem {
    pub id: String,
    pub to_repo: String,
    pub priority: String,
    pub status: String,
    pub sort_order: i32,
    pub created_at: String,
}

/// Get the next pending work item for a repo and atomically accept it.
///
/// Mirrors `kanban::next_work`'s behavior (same ordering, same atomic
/// claim), reading and writing only identity + scheduling fields.
pub fn next_work(db: &Database, repo: &str) -> Result<Option<QueueItem>> {
    db.pick_next_pending_work(repo)
}

/// Peek at the next pending work item without accepting it.
pub fn peek_work(db: &Database, repo: &str) -> Result<Option<QueueItem>> {
    db.peek_next_pending_work(repo)
}

/// Ids of every accepted work item for `repo`, in board-goal priority
/// order.
///
/// Backs `legion goal` (#934) -- see `db::queue::accepted_work_item_ids`
/// for why this stays identity-only.
pub fn accepted_work_items(db: &Database, repo: &str) -> Result<Vec<String>> {
    db.accepted_work_item_ids(repo)
}

/// Complete a work item: transition it to `done`.
///
/// Refused (not silently ignored) when the item is not currently in an
/// allowed-from status (`accepted` or `in-review`) -- see
/// `db::queue::complete_queue_work` for the guard this preserves from the
/// card-surface state machine.
pub fn complete_work(db: &Database, id: &str, note: Option<&str>) -> Result<()> {
    db.complete_queue_work(id, note)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::testutil::test_db;

    fn insert_pending(db: &Database, repo: &str) -> String {
        db.insert_card(
            "kelex",
            repo,
            "queue test item",
            None,
            crate::kanban::Priority::Med,
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
    fn next_work_claims_the_highest_priority_item() {
        let db = test_db();
        let id = insert_pending(&db, "kelex");

        let claimed = next_work(&db, "kelex").unwrap().expect("candidate");
        assert_eq!(claimed.id, id);
        assert_eq!(claimed.status, "accepted");
    }

    #[test]
    fn peek_work_does_not_claim() {
        let db = test_db();
        insert_pending(&db, "kelex");

        peek_work(&db, "kelex").unwrap().expect("candidate");
        // Still claimable after a peek.
        assert!(next_work(&db, "kelex").unwrap().is_some());
    }

    #[test]
    fn complete_work_transitions_a_claimed_item_to_done() {
        let db = test_db();
        let id = insert_pending(&db, "kelex");
        next_work(&db, "kelex").unwrap();

        complete_work(&db, &id, Some("done note")).expect("complete");
        let card = db.get_card_by_id(&id).unwrap().expect("exists");
        assert_eq!(card.status.to_string(), "done");
        assert_eq!(card.note.as_deref(), Some("done note"));
    }

    #[test]
    fn accepted_work_items_returns_ids_of_accepted_cards_only() {
        let db = test_db();
        let id = insert_pending(&db, "kelex");
        next_work(&db, "kelex").unwrap(); // claims it -> accepted
        insert_pending(&db, "kelex"); // stays pending

        let ids = accepted_work_items(&db, "kelex").unwrap();
        assert_eq!(ids, vec![id]);
    }

    #[test]
    fn complete_work_refuses_an_item_still_pending() {
        let db = test_db();
        let id = insert_pending(&db, "kelex");

        let err = complete_work(&db, &id, None).expect_err("pending item cannot complete");
        assert!(matches!(
            err,
            crate::error::LegionError::InvalidCardTransition { .. }
        ));
    }
}
