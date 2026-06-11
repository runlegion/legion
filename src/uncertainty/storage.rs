//! Database CRUD for the uncertainty engine.
//!
//! Type-aware bridge between `db::Database` and the domain types in
//! [`super::types`]. Reads route through the type constructors so the
//! `[0, 1]` newtype guarantee survives storage round-trips; writes use
//! plain rusqlite params, matching the rest of the data layer.

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
}
