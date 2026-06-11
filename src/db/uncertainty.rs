//! DDL for the uncertainty engine tables (#355). The storage methods
//! live in src/uncertainty/storage.rs, which carries its own
//! `impl Database` block; the sync delta getters live in `super::sync`.

use rusqlite::Connection;

use crate::error::Result;

/// `uncertainty_prediction` and `uncertainty_calibration_snapshot`
/// tables (#355).
pub(super) fn create_tables(conn: &Connection) -> Result<()> {
    // Migration 22: Uncertainty engine tables (#355, child of #354).
    //
    // Pillar 2 turns features from SCIP + task descriptions into a
    // calibrated prediction with a confidence bucket, witnesses the
    // outcome from usage + PR merges, and rolls calibration snapshots
    // nightly. Lifecycle: emitted -> witnessed -> calibrated -> orphaned
    // -> retired. The orphan state is load-bearing -- silence is its own
    // state, counted under the Brier uncertainty term (not reliability),
    // so unwitnessed predictions do not poison the reliability score.
    //
    // Post-correction notes from platform (Whatsonyourmind review on
    // legion#354, ack 019de0ac):
    //
    //  - bucket_lower / bucket_upper are quantile-derived (equal-frequency,
    //    10 per cohort) -- schema unchanged, but the calibration roller
    //    writes quantile bounds rather than fixed 0.1 widths.
    //  - actual_correctness stores the Empirical-Bayes Beta-posterior
    //    shrunk value used for visible calibration; actual_correctness_raw
    //    stores the unshrunk cell average for audit and back-out.
    //  - Reference: Brocker (2009), reliability/sufficiency decomposition.
    //
    // updated_at + deleted_at columns exist for smugglr LWW sync
    // (mirrors reflections / tasks / schedules).
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS uncertainty_prediction (
                id TEXT PRIMARY KEY,
                surface TEXT NOT NULL,
                feature_key TEXT NOT NULL,
                input_fingerprint TEXT NOT NULL,
                model TEXT NOT NULL,
                model_version TEXT NOT NULL,
                claimed_confidence REAL NOT NULL,
                prediction_payload TEXT NOT NULL,
                state TEXT NOT NULL DEFAULT 'emitted',
                outcome_label TEXT,
                outcome_payload TEXT,
                outcome_correctness REAL,
                cohort_key TEXT NOT NULL,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                witnessed_at TEXT,
                orphan_after TEXT,
                deleted_at TEXT
            );
            CREATE INDEX IF NOT EXISTS idx_uncertainty_prediction_cohort
                ON uncertainty_prediction(cohort_key) WHERE deleted_at IS NULL;
            CREATE INDEX IF NOT EXISTS idx_uncertainty_prediction_surface
                ON uncertainty_prediction(surface) WHERE deleted_at IS NULL;
            CREATE INDEX IF NOT EXISTS idx_uncertainty_prediction_state
                ON uncertainty_prediction(state) WHERE deleted_at IS NULL;
            CREATE INDEX IF NOT EXISTS idx_uncertainty_prediction_orphan_sweep
                ON uncertainty_prediction(state, orphan_after) WHERE deleted_at IS NULL;

            CREATE TABLE IF NOT EXISTS uncertainty_calibration_snapshot (
                id TEXT PRIMARY KEY,
                cohort_key TEXT NOT NULL,
                bucket_lower REAL NOT NULL,
                bucket_upper REAL NOT NULL,
                claimed_confidence REAL NOT NULL,
                actual_correctness REAL NOT NULL,
                actual_correctness_raw REAL NOT NULL,
                prediction_count INTEGER NOT NULL,
                orphan_count INTEGER NOT NULL,
                brier_score REAL NOT NULL,
                computed_at TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                deleted_at TEXT
            );
            CREATE INDEX IF NOT EXISTS idx_uncertainty_calibration_cohort
                ON uncertainty_calibration_snapshot(cohort_key) WHERE deleted_at IS NULL;
            CREATE INDEX IF NOT EXISTS idx_uncertainty_calibration_computed
                ON uncertainty_calibration_snapshot(computed_at) WHERE deleted_at IS NULL;",
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::db::testutil::test_db;

    #[test]
    fn uncertainty_migration_creates_both_tables() {
        let db = test_db();
        let table_count: i64 = db
            .conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' \
                 AND name IN ('uncertainty_prediction', 'uncertainty_calibration_snapshot')",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(table_count, 2);
    }

    #[test]
    fn uncertainty_migration_creates_orphan_sweep_index() {
        let db = test_db();
        let index_count: i64 = db
            .conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='index' \
                 AND name='idx_uncertainty_prediction_orphan_sweep'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(index_count, 1);
    }
}
