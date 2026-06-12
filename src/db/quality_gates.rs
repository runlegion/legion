//! Quality gates: skill-runner results keyed to commits so
//! `legion pr create` can verify a clean gate (#200). Owns the
//! `quality_gates` DDL.

use std::str::FromStr;

use chrono::Utc;
use rusqlite::Connection;
use uuid::Uuid;

use super::Database;
use crate::error::{LegionError, Result};
use crate::verify::GateResult;

/// `quality_gates` table (#200).
pub(super) fn create_tables(conn: &Connection) -> Result<()> {
    // Migration 12: Quality gates for PR creation guard (#200).
    // Records results from skill runners (e.g. legion-simplify) so
    // `legion pr create` can verify a clean gate before opening a PR.
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS quality_gates (
                id TEXT PRIMARY KEY,
                branch TEXT NOT NULL,
                commit_hash TEXT NOT NULL,
                skill TEXT NOT NULL,
                result TEXT NOT NULL,
                findings_count INTEGER NOT NULL DEFAULT 0,
                details TEXT,
                created_at TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_quality_gates_lookup
                ON quality_gates(commit_hash, skill);",
    )?;
    Ok(())
}

/// A recorded quality gate result tied to a git commit and skill.
///
/// Written by skill runners so `legion pr create` can verify clean state
/// before calling the work source. Using the DB instead of a file flag
/// prevents agents from self-reporting "clean" without proof.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct QualityGateRow {
    pub id: String,
    pub branch: String,
    pub commit_hash: String,
    pub skill: String,
    pub result: GateResult,
    pub findings_count: u64,
    pub details: Option<String>,
    pub created_at: String,
}

/// Parse a `GateResult` from a DB string, mapping unknown values to a
/// `LegionError::Database` so the error stays in the rusqlite query path.
fn parse_result_from_db(s: String) -> std::result::Result<GateResult, rusqlite::Error> {
    GateResult::from_str(&s).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(
            4,
            rusqlite::types::Type::Text,
            Box::new(std::io::Error::other(e.to_string())),
        )
    })
}

impl Database {
    /// Record a quality gate result for the given commit and skill.
    ///
    /// Multiple rows for the same (commit_hash, skill) pair are allowed --
    /// `get_quality_gate` returns the most recent one. This lets agents
    /// re-run the skill after fixing issues without losing the history.
    pub fn record_quality_gate(
        &self,
        branch: &str,
        commit_hash: &str,
        skill: &str,
        result: GateResult,
        findings_count: u64,
        details: Option<&str>,
    ) -> Result<QualityGateRow> {
        let id = Uuid::now_v7().to_string();
        let created_at = Utc::now().to_rfc3339();
        let result_str: &str = match result {
            GateResult::Clean => "clean",
            GateResult::Issues => "issues",
        };
        self.conn.execute(
            "INSERT INTO quality_gates \
             (id, branch, commit_hash, skill, result, findings_count, details, created_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            rusqlite::params![
                &id,
                branch,
                commit_hash,
                skill,
                result_str,
                findings_count as i64,
                details,
                &created_at,
            ],
        )?;
        Ok(QualityGateRow {
            id,
            branch: branch.to_owned(),
            commit_hash: commit_hash.to_owned(),
            skill: skill.to_owned(),
            result,
            findings_count,
            details: details.map(str::to_owned),
            created_at,
        })
    }

    /// Return the most recent gate row for the given (commit_hash, skill), if any.
    ///
    /// Returns `None` when no gate has been recorded for this commit. The caller
    /// should treat `None` as "gate not run" and refuse to proceed.
    pub fn get_quality_gate(
        &self,
        commit_hash: &str,
        skill: &str,
    ) -> Result<Option<QualityGateRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, branch, commit_hash, skill, result, findings_count, details, created_at \
             FROM quality_gates \
             WHERE commit_hash = ?1 AND skill = ?2 \
             ORDER BY created_at DESC \
             LIMIT 1",
        )?;
        let mut rows = stmt.query_map(rusqlite::params![commit_hash, skill], |row| {
            let result_str: String = row.get(4)?;
            let findings_count_i64: i64 = row.get(5)?;
            Ok(QualityGateRow {
                id: row.get(0)?,
                branch: row.get(1)?,
                commit_hash: row.get(2)?,
                skill: row.get(3)?,
                result: parse_result_from_db(result_str)?,
                findings_count: findings_count_i64.unsigned_abs(),
                details: row.get(6)?,
                created_at: row.get(7)?,
            })
        })?;
        match rows.next() {
            Some(Ok(row)) => Ok(Some(row)),
            Some(Err(e)) => Err(LegionError::Database(e)),
            None => Ok(None),
        }
    }

    /// Return the most recent gate row for `skill` across all commits, if any.
    ///
    /// Unlike `get_quality_gate`, this ignores the commit hash. Used by the
    /// verify gate (#520): a verify verdict is keyed on the card
    /// (`legion-verify:<card_id>`) and must still satisfy the ->Done check even
    /// when `legion done` runs on a different commit than verify did (e.g.
    /// after the branch merged). Staleness is the caller's concern -- re-verify
    /// after material changes.
    pub fn get_latest_quality_gate_by_skill(&self, skill: &str) -> Result<Option<QualityGateRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, branch, commit_hash, skill, result, findings_count, details, created_at \
             FROM quality_gates \
             WHERE skill = ?1 \
             ORDER BY created_at DESC \
             LIMIT 1",
        )?;
        let mut rows = stmt.query_map(rusqlite::params![skill], |row| {
            let result_str: String = row.get(4)?;
            let findings_count_i64: i64 = row.get(5)?;
            Ok(QualityGateRow {
                id: row.get(0)?,
                branch: row.get(1)?,
                commit_hash: row.get(2)?,
                skill: row.get(3)?,
                result: parse_result_from_db(result_str)?,
                findings_count: findings_count_i64.unsigned_abs(),
                details: row.get(6)?,
                created_at: row.get(7)?,
            })
        })?;
        match rows.next() {
            Some(Ok(row)) => Ok(Some(row)),
            Some(Err(e)) => Err(LegionError::Database(e)),
            None => Ok(None),
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::db::testutil::test_db;
    use crate::verify::GateResult;

    #[test]
    fn quality_gate_insert_and_lookup() {
        let db = test_db();
        let row = db
            .record_quality_gate(
                "feat/test-branch",
                "abc1234def5678",
                "legion-simplify",
                GateResult::Clean,
                0,
                None,
            )
            .unwrap();
        assert!(!row.id.is_empty());
        assert_eq!(row.branch, "feat/test-branch");
        assert_eq!(row.commit_hash, "abc1234def5678");
        assert_eq!(row.skill, "legion-simplify");
        assert_eq!(row.result, GateResult::Clean);
        assert_eq!(row.findings_count, 0);
        assert!(row.details.is_none());

        let fetched = db
            .get_quality_gate("abc1234def5678", "legion-simplify")
            .unwrap();
        assert!(fetched.is_some());
        let fetched = fetched.unwrap();
        assert_eq!(fetched.id, row.id);
        assert_eq!(fetched.result, GateResult::Clean);
    }

    #[test]
    fn quality_gate_missing_commit_returns_none() {
        let db = test_db();
        let result = db
            .get_quality_gate("nonexistent-hash", "legion-simplify")
            .unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn quality_gate_missing_skill_returns_none() {
        let db = test_db();
        db.record_quality_gate(
            "main",
            "abc1234",
            "legion-simplify",
            GateResult::Clean,
            0,
            None,
        )
        .unwrap();
        // Different skill on the same commit should not match.
        let result = db.get_quality_gate("abc1234", "legion-review").unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn quality_gate_multiple_skills_on_same_commit() {
        let db = test_db();
        let hash = "deadbeef12345";
        db.record_quality_gate("main", hash, "legion-simplify", GateResult::Clean, 0, None)
            .unwrap();
        db.record_quality_gate(
            "main",
            hash,
            "legion-review",
            GateResult::Issues,
            2,
            Some("{}"),
        )
        .unwrap();

        let simplify = db
            .get_quality_gate(hash, "legion-simplify")
            .unwrap()
            .expect("simplify gate should exist");
        assert_eq!(simplify.result, GateResult::Clean);

        let review = db
            .get_quality_gate(hash, "legion-review")
            .unwrap()
            .expect("review gate should exist");
        assert_eq!(review.result, GateResult::Issues);
        assert_eq!(review.findings_count, 2);
    }

    #[test]
    fn quality_gate_reruns_return_most_recent() {
        let db = test_db();
        let hash = "cafecafe99";
        // First run: issues found.
        db.record_quality_gate("main", hash, "legion-simplify", GateResult::Issues, 3, None)
            .unwrap();
        // Second run after fixing: clean.
        db.record_quality_gate("main", hash, "legion-simplify", GateResult::Clean, 0, None)
            .unwrap();

        let gate = db
            .get_quality_gate(hash, "legion-simplify")
            .unwrap()
            .expect("gate should exist");
        assert_eq!(
            gate.result,
            GateResult::Clean,
            "should return the most recent (clean) result"
        );
    }

    #[test]
    fn quality_gate_stores_details_json() {
        let db = test_db();
        let details = r#"{"result":"issues","findings_count":1,"findings":[]}"#;
        let row = db
            .record_quality_gate(
                "feat/x",
                "hash123",
                "legion-simplify",
                GateResult::Issues,
                1,
                Some(details),
            )
            .unwrap();
        assert_eq!(row.details.as_deref(), Some(details));

        let fetched = db
            .get_quality_gate("hash123", "legion-simplify")
            .unwrap()
            .unwrap();
        assert_eq!(fetched.details.as_deref(), Some(details));
    }

    #[test]
    fn latest_quality_gate_by_skill_ignores_commit() {
        // #520: the verify gate is card-keyed (legion-verify:<card>) and must
        // resolve regardless of the commit it was recorded on, since `legion
        // done` may run on a different commit than verify did.
        let db = test_db();
        let skill = "legion-verify:card-7";
        db.record_quality_gate("feat/x", "commit-old", skill, GateResult::Issues, 1, None)
            .unwrap();
        db.record_quality_gate("main", "commit-new", skill, GateResult::Clean, 0, None)
            .unwrap();

        let latest = db
            .get_latest_quality_gate_by_skill(skill)
            .unwrap()
            .expect("a verify gate should exist for the card");
        assert_eq!(
            latest.result,
            GateResult::Clean,
            "most recent row wins across commits"
        );
        assert_eq!(latest.commit_hash, "commit-new");

        // A different card's skill key does not match.
        assert!(
            db.get_latest_quality_gate_by_skill("legion-verify:card-99")
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn gate_result_display_roundtrip() {
        for r in [GateResult::Clean, GateResult::Issues] {
            let s = r.to_string();
            let parsed = s.parse::<GateResult>().expect("parse");
            assert_eq!(r, parsed);
        }
    }

    #[test]
    fn gate_result_parse_invalid_returns_err() {
        assert!("unknown".parse::<GateResult>().is_err());
        assert!("Clean".parse::<GateResult>().is_err());
        assert!("".parse::<GateResult>().is_err());
    }
}
