//! Database CRUD for the uncertainty engine.
//!
//! Type-aware bridge between `db::Database` and the domain types in
//! [`super::types`]. Reads route through the type constructors so the
//! `[0, 1]` newtype guarantee survives storage round-trips; writes use
//! plain rusqlite params, matching the rest of the data layer.

use std::collections::BTreeMap;

use chrono::Utc;
use rusqlite::params;

use crate::db::Database;

use super::error::{Result, UncertaintyError};
use super::types::{
    CalibrationSnapshot, Confidence, Correctness, OutcomeLabel, Prediction, PredictionState,
};

/// One row of the surface-grouped orphan summary.
#[derive(Debug, Clone, serde::Serialize)]
pub struct OrphanSummaryRow {
    pub surface: String,
    pub count: i64,
}

/// Outcome of one `roll_calibration` pass.
///
/// Returned to the CLI (`legion uncertainty roll`) so the operator sees
/// what the roller actually did without re-querying the snapshot table.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize)]
pub struct RollSummary {
    /// Distinct cohorts that had at least one witnessed, scored prediction.
    pub cohorts_rolled: usize,
    /// Total calibration-snapshot rows written (soft-delete-and-replace
    /// counts only the fresh rows, not the tombstoned ones).
    pub buckets_written: usize,
    /// Total witnessed predictions folded into the bucket math.
    pub predictions_scored: usize,
}

impl Database {
    /// Insert a freshly-emitted prediction. Conflict on id is a hard error.
    pub fn insert_prediction(&self, p: &Prediction) -> Result<()> {
        let payload_json = serde_json::to_string(&p.prediction_payload)?;
        self.conn.execute(
            "INSERT INTO uncertainty_prediction \
             (id, surface, feature_key, input_fingerprint, model, model_version, \
              claimed_confidence, prediction_payload, state, cohort_key, \
              created_at, updated_at, orphan_after) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
            params![
                p.id,
                p.surface,
                p.feature_key,
                p.input_fingerprint,
                p.model,
                p.model_version,
                p.claimed_confidence.value(),
                payload_json,
                p.state.as_str(),
                p.cohort_key,
                p.created_at,
                p.updated_at,
                p.orphan_after,
            ],
        )?;
        Ok(())
    }

    /// Fetch one prediction by id. None if the row does not exist or is
    /// soft-deleted.
    pub fn get_prediction(&self, id: &str) -> Result<Option<Prediction>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, surface, feature_key, input_fingerprint, model, model_version, \
             claimed_confidence, prediction_payload, state, outcome_label, outcome_payload, \
             outcome_correctness, cohort_key, created_at, updated_at, witnessed_at, \
             orphan_after \
             FROM uncertainty_prediction \
             WHERE id = ?1 AND deleted_at IS NULL",
        )?;
        let mut rows = stmt.query(params![id])?;
        if let Some(row) = rows.next()? {
            Ok(Some(map_prediction_row(row)?))
        } else {
            Ok(None)
        }
    }

    /// Find the most recent Emitted prediction for a (surface, fingerprint).
    /// Used by the gate-trust witness to resolve a (skill, commit) back to the
    /// prediction to witness. Returns None if none is in the Emitted state --
    /// the re-run contract: a re-run emits another row with the same
    /// fingerprint, so this takes the latest; earlier duplicates may already be
    /// witnessed or orphaned and are skipped.
    pub fn latest_emitted_by_fingerprint(
        &self,
        surface: &str,
        fingerprint: &str,
    ) -> Result<Option<Prediction>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, surface, feature_key, input_fingerprint, model, model_version, \
             claimed_confidence, prediction_payload, state, outcome_label, outcome_payload, \
             outcome_correctness, cohort_key, created_at, updated_at, witnessed_at, \
             orphan_after \
             FROM uncertainty_prediction \
             WHERE surface = ?1 AND input_fingerprint = ?2 AND state = 'emitted' \
             AND deleted_at IS NULL \
             ORDER BY created_at DESC, id DESC \
             LIMIT 1",
        )?;
        let mut rows = stmt.query(params![surface, fingerprint])?;
        if let Some(row) = rows.next()? {
            Ok(Some(map_prediction_row(row)?))
        } else {
            Ok(None)
        }
    }

    /// Persist a prediction whose state has advanced (witness / calibrate /
    /// orphan / retire). UPDATE keyed by id; updated_at is taken from the
    /// in-memory row so callers control the timestamp.
    pub fn update_prediction(&self, p: &Prediction) -> Result<()> {
        let payload_json = serde_json::to_string(&p.prediction_payload)?;
        let outcome_payload_json = match &p.outcome_payload {
            Some(v) => Some(serde_json::to_string(v)?),
            None => None,
        };
        let outcome_correctness_f64 = p.outcome_correctness.map(|c| c.value());
        let outcome_label_str = p.outcome_label.map(|l| l.as_str());
        let rows = self.conn.execute(
            "UPDATE uncertainty_prediction SET \
             prediction_payload = ?1, \
             state = ?2, \
             outcome_label = ?3, \
             outcome_payload = ?4, \
             outcome_correctness = ?5, \
             witnessed_at = ?6, \
             updated_at = ?7 \
             WHERE id = ?8 AND deleted_at IS NULL",
            params![
                payload_json,
                p.state.as_str(),
                outcome_label_str,
                outcome_payload_json,
                outcome_correctness_f64,
                p.witnessed_at,
                p.updated_at,
                p.id,
            ],
        )?;
        if rows == 0 {
            return Err(UncertaintyError::PredictionNotFound(p.id.clone()));
        }
        Ok(())
    }

    /// Read calibration snapshot rows, optionally filtered by surface +
    /// model. Ordered by `bucket_lower` ASC so a reliability diagram can
    /// render top-to-bottom.
    ///
    /// `surface` and `model` are matched as prefixes / interior segments of
    /// the cohort_key (`<surface>:<model>:<version>:<bucket>`). The model
    /// filter uses `%:<model>:%` which can over-match if a future surface
    /// or version legitimately contains a colon-bounded substring equal to
    /// a model name. Tighten by querying against normalized columns once
    /// the calibration roller in #359 starts producing rows at scale.
    pub fn list_calibration_snapshots(
        &self,
        surface: Option<&str>,
        model: Option<&str>,
    ) -> Result<Vec<CalibrationSnapshot>> {
        let mut clauses: Vec<String> = vec!["deleted_at IS NULL".to_string()];
        let mut binds: Vec<String> = Vec::new();

        if let Some(s) = surface {
            clauses.push(format!("cohort_key LIKE ?{}", binds.len() + 1));
            binds.push(format!("{}:%", s));
        }
        if let Some(m) = model {
            clauses.push(format!("cohort_key LIKE ?{}", binds.len() + 1));
            binds.push(format!("%:{}:%", m));
        }

        let sql = format!(
            "SELECT id, cohort_key, bucket_lower, bucket_upper, claimed_confidence, \
             actual_correctness, actual_correctness_raw, prediction_count, orphan_count, \
             brier_score, computed_at, updated_at \
             FROM uncertainty_calibration_snapshot WHERE {} ORDER BY bucket_lower ASC",
            clauses.join(" AND ")
        );

        let mut stmt = self.conn.prepare(&sql)?;
        let bind_refs: Vec<&dyn rusqlite::types::ToSql> = binds
            .iter()
            .map(|b| b as &dyn rusqlite::types::ToSql)
            .collect();
        let rows = stmt.query_map(bind_refs.as_slice(), |row| {
            Ok(CalibrationSnapshot {
                id: row.get(0)?,
                cohort_key: row.get(1)?,
                bucket_lower: row.get(2)?,
                bucket_upper: row.get(3)?,
                claimed_confidence: row.get(4)?,
                actual_correctness: row.get(5)?,
                actual_correctness_raw: row.get(6)?,
                prediction_count: row.get(7)?,
                orphan_count: row.get(8)?,
                brier_score: row.get(9)?,
                computed_at: row.get(10)?,
                updated_at: row.get(11)?,
            })
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(UncertaintyError::Database)
    }

    /// Count predictions on a surface in a given state. Read-only. Currently a
    /// test-support accessor (the gate-trust emit test uses it to prove emission
    /// positively); Phase 2b's witness lookup will promote it to a production
    /// accessor, so it is `#[cfg(test)]` for now rather than carrying a
    /// dead-code allow in the production build.
    #[cfg(test)]
    pub fn count_predictions_by_surface_state(
        &self,
        surface: &str,
        state: PredictionState,
    ) -> Result<i64> {
        let n: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM uncertainty_prediction \
             WHERE surface = ?1 AND state = ?2 AND deleted_at IS NULL",
            params![surface, state.as_str()],
            |r| r.get(0),
        )?;
        Ok(n)
    }

    /// Group orphan-state predictions by surface. Optionally filtered to a
    /// single surface. Used by the dashboard + nightly digest.
    pub fn count_orphans_by_surface(&self, surface: Option<&str>) -> Result<Vec<OrphanSummaryRow>> {
        let (sql, bind): (&str, Vec<String>) = match surface {
            Some(s) => (
                "SELECT surface, COUNT(*) as c FROM uncertainty_prediction \
                 WHERE state = 'orphaned' AND deleted_at IS NULL AND surface = ?1 \
                 GROUP BY surface ORDER BY c DESC",
                vec![s.to_string()],
            ),
            None => (
                "SELECT surface, COUNT(*) as c FROM uncertainty_prediction \
                 WHERE state = 'orphaned' AND deleted_at IS NULL \
                 GROUP BY surface ORDER BY c DESC",
                Vec::new(),
            ),
        };
        let mut stmt = self.conn.prepare(sql)?;
        let bind_refs: Vec<&dyn rusqlite::types::ToSql> = bind
            .iter()
            .map(|b| b as &dyn rusqlite::types::ToSql)
            .collect();
        let rows = stmt.query_map(bind_refs.as_slice(), |row| {
            Ok(OrphanSummaryRow {
                surface: row.get(0)?,
                count: row.get(1)?,
            })
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(UncertaintyError::Database)
    }

    /// Move stale `emitted` predictions into the `orphaned` state.
    ///
    /// A prediction orphans when its `orphan_after` deadline has passed
    /// without a witness. Only rows still in `emitted` are eligible --
    /// this cannot race a witness that already landed, because
    /// `Prediction::orphan` (and the lifecycle's `can_transition_to`) only
    /// accepts `Emitted -> Orphaned`, and the same guard is encoded in
    /// this SQL's `WHERE state = 'emitted'` clause. Uses
    /// `idx_uncertainty_prediction_orphan_sweep` (state, orphan_after).
    ///
    /// `now` is caller-supplied (not read from the clock in here) so tests
    /// can drive the sweep deterministically; the CLI passes
    /// `Utc::now().to_rfc3339()`.
    pub fn sweep_orphans(&self, now: &str) -> Result<usize> {
        let rows = self.conn.execute(
            "UPDATE uncertainty_prediction SET state = 'orphaned', updated_at = ?1 \
             WHERE state = 'emitted' AND orphan_after IS NOT NULL AND orphan_after < ?1 \
             AND deleted_at IS NULL",
            params![now],
        )?;
        Ok(rows)
    }

    /// Roll fresh calibration snapshots for every cohort with witnessed
    /// data.
    ///
    /// MINIMAL ESTIMATOR (#359): fixed-width 0.1 point-Brier, no
    /// Empirical-Bayes shrinkage. `actual_correctness` and
    /// `actual_correctness_raw` are written identically -- the shrunk
    /// column exists in the schema for a future EB-Beta upgrade
    /// (platform's post-correction notes, see `db/uncertainty.rs`), but
    /// this roller deliberately does not implement it yet. Point Brier
    /// per bucket: `mean((claimed_confidence_i - outcome_correctness_i)^2)`
    /// over the bucket's witnessed predictions.
    ///
    /// For each distinct `cohort_key` with at least one witnessed,
    /// scored prediction (`state = 'witnessed' AND outcome_correctness IS
    /// NOT NULL`), the cohort's witnessed predictions are grouped into
    /// fixed [0.0,0.1) .. [0.9,1.0] buckets by `claimed_confidence`
    /// (last bucket closed on both ends). Orphaned predictions are
    /// excluded from the bucket math -- they only contribute the
    /// cohort-wide `orphan_count` recorded on every bucket row for that
    /// cohort, per the schema's "silence is its own state, counted under
    /// the Brier uncertainty term, not reliability" invariant.
    ///
    /// Idempotent replace, not append: existing live snapshot rows for a
    /// touched cohort are soft-deleted (`deleted_at = updated_at = now`)
    /// and fresh rows inserted in the same transaction, mirroring the
    /// soft-delete + `updated_at`/`deleted_at` LWW convention the sync
    /// deltas already read (`db::sync::get_uncertainty_calibration_snapshot_deltas_since`).
    /// Re-running with unchanged inputs produces a new live row set with
    /// identical content (new ids, same bucket math) -- callers should
    /// compare by `(cohort_key, bucket_lower)`, not by id.
    pub fn roll_calibration(&self, now: &str) -> Result<RollSummary> {
        let cohort_keys: Vec<String> = {
            let mut stmt = self.conn.prepare(
                "SELECT DISTINCT cohort_key FROM uncertainty_prediction \
                 WHERE state = 'witnessed' AND outcome_correctness IS NOT NULL \
                 AND deleted_at IS NULL",
            )?;
            let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
            rows.collect::<std::result::Result<Vec<_>, _>>()
                .map_err(UncertaintyError::Database)?
        };

        let mut summary = RollSummary::default();

        let tx = self.conn.unchecked_transaction()?;
        {
            let mut fetch_stmt = tx.prepare(
                "SELECT claimed_confidence, outcome_correctness FROM uncertainty_prediction \
                 WHERE cohort_key = ?1 AND state = 'witnessed' \
                 AND outcome_correctness IS NOT NULL AND deleted_at IS NULL",
            )?;
            let mut orphan_count_stmt = tx.prepare(
                "SELECT COUNT(*) FROM uncertainty_prediction \
                 WHERE cohort_key = ?1 AND state = 'orphaned' AND deleted_at IS NULL",
            )?;
            let mut soft_delete_stmt = tx.prepare(
                "UPDATE uncertainty_calibration_snapshot SET deleted_at = ?1, updated_at = ?1 \
                 WHERE cohort_key = ?2 AND deleted_at IS NULL",
            )?;
            let mut insert_stmt = tx.prepare(
                "INSERT INTO uncertainty_calibration_snapshot \
                 (id, cohort_key, bucket_lower, bucket_upper, claimed_confidence, \
                  actual_correctness, actual_correctness_raw, prediction_count, \
                  orphan_count, brier_score, computed_at, updated_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6, ?7, ?8, ?9, ?10, ?10)",
            )?;

            for cohort_key in &cohort_keys {
                let pairs: Vec<(f64, f64)> = fetch_stmt
                    .query_map(params![cohort_key], |row| {
                        Ok((row.get::<_, f64>(0)?, row.get::<_, f64>(1)?))
                    })?
                    .collect::<std::result::Result<Vec<_>, _>>()
                    .map_err(UncertaintyError::Database)?;

                if pairs.is_empty() {
                    continue;
                }

                let orphan_count: i64 =
                    orphan_count_stmt.query_row(params![cohort_key], |row| row.get(0))?;

                // Fixed-width 0.1 bucketing: idx 0..=9, last bucket closed on
                // both ends (a claimed_confidence of exactly 1.0 floors to
                // idx 10 before the clamp, landing in bucket 9's [0.9, 1.0]).
                let mut buckets: BTreeMap<i64, Vec<(f64, f64)>> = BTreeMap::new();
                for (claimed, outcome) in &pairs {
                    let idx = ((claimed * 10.0).floor() as i64).clamp(0, 9);
                    buckets.entry(idx).or_default().push((*claimed, *outcome));
                }

                soft_delete_stmt.execute(params![now, cohort_key])?;

                for (idx, members) in &buckets {
                    let n = members.len() as f64;
                    let sum_claimed: f64 = members.iter().map(|(c, _)| c).sum();
                    let sum_outcome: f64 = members.iter().map(|(_, o)| o).sum();
                    let sum_sq_err: f64 = members.iter().map(|(c, o)| (c - o).powi(2)).sum();

                    let bucket_lower = *idx as f64 / 10.0;
                    let bucket_upper = if *idx == 9 {
                        1.0
                    } else {
                        (*idx + 1) as f64 / 10.0
                    };
                    let claimed_mean = sum_claimed / n;
                    // No EB shrinkage in the minimal roller: actual_correctness
                    // and actual_correctness_raw are the same raw cell mean.
                    let actual_mean = sum_outcome / n;
                    let brier = sum_sq_err / n;

                    let id = uuid::Uuid::now_v7().to_string();
                    insert_stmt.execute(params![
                        id,
                        cohort_key,
                        bucket_lower,
                        bucket_upper,
                        claimed_mean,
                        actual_mean,
                        members.len() as i64,
                        orphan_count,
                        brier,
                        now,
                    ])?;
                    summary.buckets_written += 1;
                }

                summary.cohorts_rolled += 1;
                summary.predictions_scored += pairs.len();
            }
        }
        tx.commit()?;

        Ok(summary)
    }
}

fn map_prediction_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Prediction> {
    let claimed_confidence_raw: f64 = row.get(6)?;
    let payload_str: String = row.get(7)?;
    let payload: serde_json::Value = serde_json::from_str(&payload_str).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(7, rusqlite::types::Type::Text, Box::new(e))
    })?;
    let state_str: String = row.get(8)?;
    let state = PredictionState::from_str(&state_str).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(8, rusqlite::types::Type::Text, Box::new(e))
    })?;
    let outcome_label_opt: Option<String> = row.get(9)?;
    let outcome_label = match outcome_label_opt {
        Some(s) => Some(OutcomeLabel::from_str(&s).map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(9, rusqlite::types::Type::Text, Box::new(e))
        })?),
        None => None,
    };
    let outcome_payload_str: Option<String> = row.get(10)?;
    let outcome_payload = match outcome_payload_str {
        Some(s) => Some(serde_json::from_str(&s).map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(10, rusqlite::types::Type::Text, Box::new(e))
        })?),
        None => None,
    };
    let outcome_correctness_raw: Option<f64> = row.get(11)?;
    let outcome_correctness = match outcome_correctness_raw {
        Some(c) => Some(Correctness::from_f64(c).map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(11, rusqlite::types::Type::Real, Box::new(e))
        })?),
        None => None,
    };
    let claimed_confidence = Confidence::from_f64(claimed_confidence_raw).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(6, rusqlite::types::Type::Real, Box::new(e))
    })?;

    Ok(Prediction {
        id: row.get(0)?,
        surface: row.get(1)?,
        feature_key: row.get(2)?,
        input_fingerprint: row.get(3)?,
        model: row.get(4)?,
        model_version: row.get(5)?,
        claimed_confidence,
        prediction_payload: payload,
        state,
        outcome_label,
        outcome_payload,
        outcome_correctness,
        cohort_key: row.get(12)?,
        created_at: row.get(13)?,
        updated_at: row.get(14)?,
        witnessed_at: row.get(15)?,
        orphan_after: row.get(16)?,
    })
}

/// Compute the ISO 8601 timestamp `days` days in the future. Used by the
/// emit CLI to derive `orphan_after` from `--orphan-ttl-days`.
pub fn orphan_after_from_ttl(days: u32) -> Option<String> {
    if days == 0 {
        return None;
    }
    let when = Utc::now() + chrono::Duration::days(days as i64);
    Some(when.to_rfc3339())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::testutil::test_db;
    use crate::uncertainty::types::{Confidence, PredictionInput};

    fn fresh_input() -> PredictionInput {
        PredictionInput {
            surface: "legion.task".into(),
            feature_key: "scip.refactor".into(),
            input_fingerprint: "fp-1".into(),
            model: "claude-opus-4-7".into(),
            model_version: "4.7".into(),
            claimed_confidence: Confidence::from_f64(0.7).unwrap(),
            prediction_payload: serde_json::json!({ "predicted_tokens": 1500 }),
            orphan_after: Some("2026-06-12T00:00:00+00:00".into()),
        }
    }

    #[test]
    fn insert_and_get_prediction_round_trips() {
        let db = test_db();
        let p = Prediction::new(fresh_input());
        db.insert_prediction(&p).unwrap();
        let fetched = db.get_prediction(&p.id).unwrap().unwrap();
        assert_eq!(fetched.id, p.id);
        assert_eq!(fetched.surface, "legion.task");
        assert_eq!(fetched.state, PredictionState::Emitted);
        assert_eq!(fetched.claimed_confidence.value(), 0.7);
        assert!(fetched.outcome_correctness.is_none());
    }

    #[test]
    fn get_prediction_missing_returns_none() {
        let db = test_db();
        let none = db.get_prediction("nope").unwrap();
        assert!(none.is_none());
    }

    #[test]
    fn update_prediction_persists_state_transition() {
        let db = test_db();
        let mut p = Prediction::new(fresh_input());
        db.insert_prediction(&p).unwrap();
        p.witness(
            OutcomeLabel::Shipped,
            serde_json::json!({ "actual_tokens": 1400 }),
            Correctness::from_f64(0.95).unwrap(),
            "2026-05-12T10:00:00+00:00",
        )
        .unwrap();
        db.update_prediction(&p).unwrap();
        let fetched = db.get_prediction(&p.id).unwrap().unwrap();
        assert_eq!(fetched.state, PredictionState::Witnessed);
        assert_eq!(fetched.outcome_label, Some(OutcomeLabel::Shipped));
        assert_eq!(fetched.outcome_correctness.map(|c| c.value()), Some(0.95));
    }

    #[test]
    fn update_prediction_missing_returns_not_found() {
        let db = test_db();
        let p = Prediction::new(fresh_input());
        let err = db.update_prediction(&p).unwrap_err();
        assert!(matches!(err, UncertaintyError::PredictionNotFound(_)));
    }

    #[test]
    fn insert_prediction_rejects_duplicate_id() {
        let db = test_db();
        let p = Prediction::new(fresh_input());
        db.insert_prediction(&p).unwrap();
        let err = db.insert_prediction(&p).unwrap_err();
        assert!(matches!(err, UncertaintyError::Database(_)));
    }

    #[test]
    fn count_orphans_groups_by_surface() {
        let db = test_db();
        // Each prediction is constructed Emitted, transitioned to Orphaned,
        // then inserted: the row lands with state='orphaned' directly.
        for _ in 0..3 {
            let mut p = Prediction::new(fresh_input());
            p.orphan("2026-05-12T10:00:00+00:00").unwrap();
            db.insert_prediction(&p).unwrap();
        }
        let mut other = fresh_input();
        other.surface = "legion.review".into();
        let mut p = Prediction::new(other);
        p.orphan("2026-05-12T10:00:00+00:00").unwrap();
        db.insert_prediction(&p).unwrap();

        let all = db.count_orphans_by_surface(None).unwrap();
        let task_row = all.iter().find(|r| r.surface == "legion.task").unwrap();
        let review_row = all.iter().find(|r| r.surface == "legion.review").unwrap();
        assert_eq!(task_row.count, 3);
        assert_eq!(review_row.count, 1);

        let filtered = db.count_orphans_by_surface(Some("legion.task")).unwrap();
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].count, 3);
    }

    #[test]
    fn orphan_after_zero_returns_none() {
        assert!(orphan_after_from_ttl(0).is_none());
    }

    #[test]
    fn orphan_after_nonzero_returns_some() {
        let s = orphan_after_from_ttl(30).unwrap();
        // Parse round-trips through chrono cleanly.
        let parsed = chrono::DateTime::parse_from_rfc3339(&s).unwrap();
        assert!(parsed > chrono::Utc::now());
    }

    #[test]
    fn list_calibration_snapshots_empty_db_returns_empty() {
        let db = test_db();
        let snaps = db.list_calibration_snapshots(None, None).unwrap();
        assert!(snaps.is_empty());
    }

    #[test]
    fn latest_emitted_by_fingerprint_finds_emitted_skips_witnessed_and_unknown() {
        let db = test_db();
        let p = Prediction::new(fresh_input()); // surface legion.task, fingerprint fp-1
        db.insert_prediction(&p).unwrap();
        // Found while Emitted.
        let found = db
            .latest_emitted_by_fingerprint("legion.task", "fp-1")
            .unwrap();
        assert_eq!(found.as_ref().map(|x| x.id.as_str()), Some(p.id.as_str()));
        // Unknown fingerprint -> None.
        assert!(
            db.latest_emitted_by_fingerprint("legion.task", "nope")
                .unwrap()
                .is_none()
        );
        // Once witnessed it is no longer Emitted -> the lookup skips it.
        let mut p2 = db.get_prediction(&p.id).unwrap().unwrap();
        p2.witness(
            OutcomeLabel::Shipped,
            serde_json::json!({}),
            Correctness::from_f64(1.0).unwrap(),
            "2026-06-29T00:00:00+00:00",
        )
        .unwrap();
        db.update_prediction(&p2).unwrap();
        assert!(
            db.latest_emitted_by_fingerprint("legion.task", "fp-1")
                .unwrap()
                .is_none()
        );
    }

    // --- sweep_orphans / roll_calibration (#359) ---

    fn seed_witnessed(
        db: &crate::db::Database,
        surface: &str,
        model: &str,
        model_version: &str,
        claimed: f64,
        outcome: f64,
    ) -> Prediction {
        let input = PredictionInput {
            surface: surface.into(),
            feature_key: "test.feature".into(),
            input_fingerprint: format!("fp-{claimed}-{outcome}-{}", uuid::Uuid::now_v7()),
            model: model.into(),
            model_version: model_version.into(),
            claimed_confidence: Confidence::from_f64(claimed).unwrap(),
            prediction_payload: serde_json::json!({}),
            orphan_after: None,
        };
        let mut p = Prediction::new(input);
        db.insert_prediction(&p).unwrap();
        p.witness(
            OutcomeLabel::Shipped,
            serde_json::json!({}),
            Correctness::from_f64(outcome).unwrap(),
            "2026-06-01T00:00:00+00:00",
        )
        .unwrap();
        db.update_prediction(&p).unwrap();
        p
    }

    fn seed_orphaned(
        db: &crate::db::Database,
        surface: &str,
        model: &str,
        model_version: &str,
        claimed: f64,
    ) -> Prediction {
        let input = PredictionInput {
            surface: surface.into(),
            feature_key: "test.feature".into(),
            input_fingerprint: format!("fp-orphan-{claimed}-{}", uuid::Uuid::now_v7()),
            model: model.into(),
            model_version: model_version.into(),
            claimed_confidence: Confidence::from_f64(claimed).unwrap(),
            prediction_payload: serde_json::json!({}),
            orphan_after: Some("2026-05-01T00:00:00+00:00".into()),
        };
        let mut p = Prediction::new(input);
        p.orphan("2026-06-01T00:00:00+00:00").unwrap();
        db.insert_prediction(&p).unwrap();
        p
    }

    #[test]
    fn sweep_orphans_marks_only_emitted_past_orphan_after() {
        let db = test_db();
        let past = "2026-05-01T00:00:00+00:00";
        let future = "2026-07-01T00:00:00+00:00";
        let now = "2026-06-01T00:00:00+00:00";

        // A: emitted, orphan_after in the past -> swept.
        let mut a_input = fresh_input();
        a_input.orphan_after = Some(past.into());
        let a = Prediction::new(a_input);
        db.insert_prediction(&a).unwrap();

        // B: emitted, orphan_after in the future -> untouched.
        let mut b_input = fresh_input();
        b_input.orphan_after = Some(future.into());
        let b = Prediction::new(b_input);
        db.insert_prediction(&b).unwrap();

        // C: witnessed, orphan_after in the past -> untouched (state guard
        // stops the sweep from racing/reclassifying a witnessed row).
        let mut c_input = fresh_input();
        c_input.orphan_after = Some(past.into());
        let mut c = Prediction::new(c_input);
        db.insert_prediction(&c).unwrap();
        c.witness(
            OutcomeLabel::Shipped,
            serde_json::json!({}),
            Correctness::from_f64(1.0).unwrap(),
            now,
        )
        .unwrap();
        db.update_prediction(&c).unwrap();

        // D: emitted, orphan_after None -> sweep disabled for this row.
        let mut d_input = fresh_input();
        d_input.orphan_after = None;
        let d = Prediction::new(d_input);
        db.insert_prediction(&d).unwrap();

        let swept = db.sweep_orphans(now).unwrap();
        assert_eq!(swept, 1);

        assert_eq!(
            db.get_prediction(&a.id).unwrap().unwrap().state,
            PredictionState::Orphaned
        );
        assert_eq!(
            db.get_prediction(&b.id).unwrap().unwrap().state,
            PredictionState::Emitted
        );
        assert_eq!(
            db.get_prediction(&c.id).unwrap().unwrap().state,
            PredictionState::Witnessed
        );
        assert_eq!(
            db.get_prediction(&d.id).unwrap().unwrap().state,
            PredictionState::Emitted
        );
    }

    #[test]
    fn sweep_orphans_ignores_soft_deleted_rows() {
        let db = test_db();
        let mut input = fresh_input();
        input.orphan_after = Some("2026-05-01T00:00:00+00:00".into());
        let p = Prediction::new(input);
        db.insert_prediction(&p).unwrap();
        db.conn
            .execute(
                "UPDATE uncertainty_prediction SET deleted_at = ?1 WHERE id = ?2",
                params!["2026-05-15T00:00:00+00:00", p.id],
            )
            .unwrap();

        let swept = db.sweep_orphans("2026-06-01T00:00:00+00:00").unwrap();
        assert_eq!(swept, 0);
    }

    #[test]
    fn roll_calibration_computes_bucket_means_and_point_brier() {
        let db = test_db();
        // Both land in fixed bucket idx 9 ([0.9, 1.0]) and share a cohort_key
        // (same surface/model/model_version/emit-bucket).
        seed_witnessed(&db, "legion.task", "claude-opus-4-7", "4.7", 0.9, 1.0);
        seed_witnessed(&db, "legion.task", "claude-opus-4-7", "4.7", 0.95, 0.6);
        // Orphaned prediction in the same cohort: must not move the bucket
        // math, but must show up in orphan_count.
        seed_orphaned(&db, "legion.task", "claude-opus-4-7", "4.7", 0.92);

        let summary = db.roll_calibration("2026-06-02T00:00:00+00:00").unwrap();
        assert_eq!(summary.cohorts_rolled, 1);
        assert_eq!(summary.buckets_written, 1);
        assert_eq!(summary.predictions_scored, 2);

        let snaps = db
            .list_calibration_snapshots(Some("legion.task"), Some("claude-opus-4-7"))
            .unwrap();
        assert_eq!(snaps.len(), 1);
        let snap = &snaps[0];
        assert_eq!(snap.prediction_count, 2);
        assert_eq!(snap.orphan_count, 1);
        assert!((snap.bucket_lower - 0.9).abs() < 1e-9);
        assert!((snap.bucket_upper - 1.0).abs() < 1e-9);
        // Hand-verified: claimed mean = (0.9 + 0.95) / 2 = 0.925.
        assert!((snap.claimed_confidence - 0.925).abs() < 1e-9);
        // Hand-verified: actual mean = (1.0 + 0.6) / 2 = 0.8.
        assert!((snap.actual_correctness - 0.8).abs() < 1e-9);
        // No EB shrinkage in the minimal roller: raw == shrunk.
        assert_eq!(snap.actual_correctness, snap.actual_correctness_raw);
        // Hand-verified point Brier: mean((0.9-1.0)^2, (0.95-0.6)^2)
        //   = mean(0.01, 0.1225) = 0.06625.
        assert!((snap.brier_score - 0.06625).abs() < 1e-9);
    }

    #[test]
    fn roll_calibration_perfectly_calibrated_cohort_yields_zero_brier() {
        let db = test_db();
        seed_witnessed(&db, "legion.review", "claude-opus-4-7", "4.7", 0.5, 0.5);
        seed_witnessed(&db, "legion.review", "claude-opus-4-7", "4.7", 0.5, 0.5);

        let summary = db.roll_calibration("2026-06-02T00:00:00+00:00").unwrap();
        assert_eq!(summary.cohorts_rolled, 1);

        let snaps = db
            .list_calibration_snapshots(Some("legion.review"), None)
            .unwrap();
        assert_eq!(snaps.len(), 1);
        assert_eq!(snaps[0].brier_score, 0.0);
        assert_eq!(snaps[0].orphan_count, 0);
    }

    #[test]
    fn roll_calibration_excludes_orphans_from_bucket_math() {
        let db = test_db();
        seed_witnessed(&db, "legion.gate", "claude-opus-4-7", "4.7", 0.9, 0.9);
        seed_orphaned(&db, "legion.gate", "claude-opus-4-7", "4.7", 0.91);
        seed_orphaned(&db, "legion.gate", "claude-opus-4-7", "4.7", 0.93);

        let summary = db.roll_calibration("2026-06-02T00:00:00+00:00").unwrap();
        // Only the single witnessed prediction is scored.
        assert_eq!(summary.predictions_scored, 1);

        let snaps = db
            .list_calibration_snapshots(Some("legion.gate"), None)
            .unwrap();
        assert_eq!(snaps.len(), 1);
        assert_eq!(snaps[0].prediction_count, 1);
        assert_eq!(snaps[0].orphan_count, 2);
    }

    #[test]
    fn roll_calibration_is_idempotent() {
        let db = test_db();
        seed_witnessed(&db, "legion.task", "claude-opus-4-7", "4.7", 0.7, 0.8);
        seed_witnessed(&db, "legion.task", "claude-opus-4-7", "4.7", 0.72, 0.6);

        db.roll_calibration("2026-06-02T00:00:00+00:00").unwrap();
        let first = db
            .list_calibration_snapshots(Some("legion.task"), None)
            .unwrap();

        db.roll_calibration("2026-06-03T00:00:00+00:00").unwrap();
        let second = db
            .list_calibration_snapshots(Some("legion.task"), None)
            .unwrap();

        assert_eq!(first.len(), 1);
        assert_eq!(second.len(), 1, "re-roll must not accumulate live rows");
        // Content is identical (ids differ -- soft-delete + replace mints a
        // fresh row each pass); compare by the fields that matter.
        assert_eq!(first[0].cohort_key, second[0].cohort_key);
        assert_eq!(first[0].bucket_lower, second[0].bucket_lower);
        assert_eq!(first[0].bucket_upper, second[0].bucket_upper);
        assert_eq!(first[0].prediction_count, second[0].prediction_count);
        assert_eq!(first[0].claimed_confidence, second[0].claimed_confidence);
        assert_eq!(first[0].actual_correctness, second[0].actual_correctness);
        assert_eq!(first[0].brier_score, second[0].brier_score);

        // The tombstoned first-pass row must still be present (soft delete,
        // not hard delete) so sync deltas can propagate the tombstone.
        let raw_count: i64 = db
            .conn
            .query_row(
                "SELECT COUNT(*) FROM uncertainty_calibration_snapshot WHERE cohort_key = ?1",
                params![first[0].cohort_key],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(raw_count, 2);
    }

    #[test]
    fn roll_calibration_no_witnessed_predictions_is_a_noop() {
        let db = test_db();
        let summary = db.roll_calibration("2026-06-02T00:00:00+00:00").unwrap();
        assert_eq!(summary.cohorts_rolled, 0);
        assert_eq!(summary.buckets_written, 0);
        assert_eq!(summary.predictions_scored, 0);
        assert!(
            db.list_calibration_snapshots(None, None)
                .unwrap()
                .is_empty()
        );
    }
}
