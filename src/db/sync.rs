//! Tombstone cleanup for synced tables. Replication itself is smugglr-core's
//! multicast gossip (see `sync_actor`); this file only hard-deletes soft
//! deleted rows once they have had time to propagate.

use chrono::Utc;

use super::Database;
use crate::error::Result;

/// Result of tombstone cleanup operation.
#[derive(Debug, Default)]
pub struct TombstoneCleanupResult {
    pub reflections: u64,
    pub tasks: u64,
    pub schedules: u64,
    pub uncertainty_predictions: u64,
    pub uncertainty_calibration_snapshots: u64,
}

impl TombstoneCleanupResult {
    pub fn total(&self) -> u64 {
        self.reflections
            + self.tasks
            + self.schedules
            + self.uncertainty_predictions
            + self.uncertainty_calibration_snapshots
    }

    pub fn is_empty(&self) -> bool {
        self.total() == 0
    }
}

impl Database {
    /// Hard-delete tombstones older than the given number of days.
    ///
    /// Removes soft-deleted rows (where deleted_at IS NOT NULL) that are older
    /// than `retention_days`. Returns a struct with counts of deleted rows per table.
    ///
    /// This is the housekeeper cleanup for multi-node sync. Once tombstones have
    /// propagated to all nodes (typically within hours), they can be permanently
    /// removed to reclaim space. A 30-day retention is recommended.
    pub fn cleanup_tombstones(&self, retention_days: i64) -> Result<TombstoneCleanupResult> {
        let cutoff = (Utc::now() - chrono::Duration::days(retention_days)).to_rfc3339();

        let reflections = self.conn.execute(
            "DELETE FROM reflections WHERE deleted_at IS NOT NULL AND deleted_at < ?1",
            [&cutoff],
        )? as u64;

        let tasks = self.conn.execute(
            "DELETE FROM tasks WHERE deleted_at IS NOT NULL AND deleted_at < ?1",
            [&cutoff],
        )? as u64;

        let schedules = self.conn.execute(
            "DELETE FROM schedules WHERE deleted_at IS NOT NULL AND deleted_at < ?1",
            [&cutoff],
        )? as u64;

        let uncertainty_predictions = self.conn.execute(
            "DELETE FROM uncertainty_prediction WHERE deleted_at IS NOT NULL AND deleted_at < ?1",
            [&cutoff],
        )? as u64;

        let uncertainty_calibration_snapshots = self.conn.execute(
            "DELETE FROM uncertainty_calibration_snapshot \
             WHERE deleted_at IS NOT NULL AND deleted_at < ?1",
            [&cutoff],
        )? as u64;

        Ok(TombstoneCleanupResult {
            reflections,
            tasks,
            schedules,
            uncertainty_predictions,
            uncertainty_calibration_snapshots,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::db::testutil::test_db;

    #[test]
    fn cleanup_tombstones_removes_old_soft_deleted_rows() {
        let db = test_db();

        // Insert and soft delete a reflection.
        let r = db.insert_reflection("kelex", "to delete", "self").unwrap();
        db.soft_delete_reflection(&r.id).unwrap();

        // Insert and soft delete a task.
        let task_id = db
            .insert_task("kelex", "legion", "to delete", None, "med")
            .unwrap();
        db.soft_delete_task(&task_id).unwrap();

        // Insert and soft delete a schedule.
        let sched_id = db
            .insert_schedule("to-delete", "*/5m", "echo bye", "legion", None, None)
            .unwrap();
        db.soft_delete_schedule(&sched_id).unwrap();

        // Cleanup with 0-day retention should remove all tombstones.
        let result = db.cleanup_tombstones(0).unwrap();
        assert_eq!(result.reflections, 1);
        assert_eq!(result.tasks, 1);
        assert_eq!(result.schedules, 1);
        assert_eq!(result.total(), 3);

        // Running again should return zeros.
        let result2 = db.cleanup_tombstones(0).unwrap();
        assert!(result2.is_empty());
    }

    #[test]
    fn cleanup_tombstones_respects_retention_period() {
        let db = test_db();

        // Insert and soft delete a reflection.
        let r = db
            .insert_reflection("kelex", "recent delete", "self")
            .unwrap();
        db.soft_delete_reflection(&r.id).unwrap();

        // Cleanup with 30-day retention should NOT remove the freshly deleted row.
        let result = db.cleanup_tombstones(30).unwrap();
        assert!(
            result.is_empty(),
            "fresh tombstone should not be cleaned up"
        );
    }

    fn insert_prediction_row(db: &Database, id: &str, updated_at: &str) {
        db.conn
            .execute(
                "INSERT INTO uncertainty_prediction \
                 (id, surface, feature_key, input_fingerprint, model, model_version, \
                  claimed_confidence, prediction_payload, state, cohort_key, \
                  created_at, updated_at) \
                 VALUES (?1, 'legion.task', 'scip.refactor', 'fp-1', 'claude-opus-4-7', \
                         '4.7', 0.7, '{\"predicted_tokens\":1000}', 'emitted', \
                         'legion:claude-opus-4-7:scip.refactor:0.7', ?2, ?3)",
                rusqlite::params![id, updated_at, updated_at],
            )
            .unwrap();
    }

    fn insert_snapshot_row(db: &Database, id: &str, updated_at: &str) {
        db.conn
            .execute(
                "INSERT INTO uncertainty_calibration_snapshot \
                 (id, cohort_key, bucket_lower, bucket_upper, claimed_confidence, \
                  actual_correctness, actual_correctness_raw, prediction_count, \
                  orphan_count, brier_score, computed_at, updated_at) \
                 VALUES (?1, 'legion:claude-opus-4-7:scip.refactor:0.7', 0.6, 0.8, 0.7, \
                         0.68, 0.65, 42, 3, 0.09, ?2, ?2)",
                rusqlite::params![id, updated_at],
            )
            .unwrap();
    }

    #[test]
    fn cleanup_tombstones_includes_uncertainty_tables() {
        let db = test_db();
        let old = (Utc::now() - chrono::Duration::days(100)).to_rfc3339();
        insert_prediction_row(&db, "p-old", &old);
        db.conn
            .execute(
                "UPDATE uncertainty_prediction SET deleted_at = ?1 WHERE id = 'p-old'",
                [&old],
            )
            .unwrap();
        insert_snapshot_row(&db, "s-old", &old);
        db.conn
            .execute(
                "UPDATE uncertainty_calibration_snapshot SET deleted_at = ?1 WHERE id = 's-old'",
                [&old],
            )
            .unwrap();

        let result = db.cleanup_tombstones(30).unwrap();
        assert_eq!(result.uncertainty_predictions, 1);
        assert_eq!(result.uncertainty_calibration_snapshots, 1);
    }
}
