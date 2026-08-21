//! Deferral: put a work item off until a future time, independent of any
//! card (#934). The legion-specific counterpart to `kanban::defer_card` /
//! `kanban::undefer_card` -- same idea (self-defer with an auto-revert
//! wake), but keyed on an opaque work-item id and a repo instead of a card
//! row, so it works whether or not a card exists behind the id.
//!
//! No work-source (GitHub issues, etc.) has a field for "wake me up at
//! time T" -- this is why defer is classified MOVE rather than
//! replace-with-native in #934's classification of the card surface.

use crate::db::Database;
use crate::error::{LegionError, Result};

/// A repo's active deferral for one work item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Deferral {
    pub work_item_id: String,
    pub repo: String,
    pub wake_at: String,
    pub note: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

/// Defer `work_item_id` (owned by `repo`) until `until`.
///
/// `until` uses the same forward-parsing grammar `legion kanban defer
/// --until` accepts (`YYYY-MM-DD`, `<N>d`, `<N>w`, `today`, via
/// `timerange::parse_point_in_time`) and must resolve to a time strictly
/// after now -- refused with `DeferWakeAtInPast` otherwise, the same error
/// the card-scoped path uses (#816), naming both the raw input and the
/// resolved timestamp so a stale absolute date is diagnosable from the
/// error alone. Parsing and validation live here, not split between this
/// function and its callers, so every caller (CLI or otherwise) gets the
/// same refusal for the same input.
///
/// Idempotent as a re-defer: calling this again for a work item that is
/// already deferred updates `wake_at`/`note` in place rather than erroring.
pub fn defer_work_item(
    db: &Database,
    work_item_id: &str,
    repo: &str,
    until: &str,
    note: Option<&str>,
) -> Result<()> {
    let wake_at = crate::timerange::parse_point_in_time(until)?;
    let now = chrono::Utc::now().to_rfc3339();
    if wake_at <= now {
        return Err(LegionError::DeferWakeAtInPast {
            input: until.to_string(),
            wake_at,
        });
    }
    db.upsert_deferral(work_item_id, repo, &wake_at, note)
}

/// Wake a deferred work item early, or as part of the scheduled sweep.
///
/// Returns the cleared `Deferral` (its last known `repo`/`wake_at`/`note`)
/// when one existed, or `None` when the work item was not deferred --
/// a no-op, not an error, mirroring `clear_deferral`'s contract.
pub fn undefer_work_item(db: &Database, work_item_id: &str) -> Result<Option<Deferral>> {
    let existing = db.get_deferral(work_item_id)?;
    if existing.is_some() {
        db.clear_deferral(work_item_id)?;
    }
    Ok(existing)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::testutil::test_db;

    #[test]
    fn defer_then_undefer_round_trips() {
        let db = test_db();
        defer_work_item(&db, "item-1", "legion", "2099-01-01", None).expect("defer");
        assert!(db.get_deferral("item-1").unwrap().is_some());

        let cleared = undefer_work_item(&db, "item-1").expect("undefer");
        assert!(cleared.is_some());
        assert!(db.get_deferral("item-1").unwrap().is_none());
    }

    #[test]
    fn defer_refuses_a_past_wake_at() {
        let db = test_db();
        let err = defer_work_item(&db, "item-1", "legion", "2020-01-01", None)
            .expect_err("past wake_at must be refused");
        match err {
            LegionError::DeferWakeAtInPast { input, .. } => assert_eq!(input, "2020-01-01"),
            other => panic!("expected DeferWakeAtInPast, got {other:?}"),
        }
    }

    #[test]
    fn defer_rejects_unparseable_until() {
        let db = test_db();
        let err = defer_work_item(&db, "item-1", "legion", "not a date", None)
            .expect_err("unparseable --until must be refused");
        assert!(matches!(err, LegionError::InvalidDateFilter { .. }));
    }

    #[test]
    fn undefer_on_a_work_item_that_was_never_deferred_is_a_no_op() {
        let db = test_db();
        let result = undefer_work_item(&db, "never-deferred").expect("no-op, not an error");
        assert!(result.is_none());
    }

    #[test]
    fn re_defer_updates_wake_at_in_place() {
        let db = test_db();
        defer_work_item(&db, "item-1", "legion", "2099-01-01", None).expect("first defer");
        defer_work_item(&db, "item-1", "legion", "2099-06-01", Some("pushed back"))
            .expect("re-defer");

        let deferral = db.get_deferral("item-1").unwrap().expect("exists");
        assert!(deferral.wake_at.starts_with("2099-06-01"));
        assert_eq!(deferral.note.as_deref(), Some("pushed back"));
    }
}
