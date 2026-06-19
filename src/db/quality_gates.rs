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
        let result_str: &str = result.as_str();
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

/// Filter parameters for `list_quality_gates`.
///
/// All fields are optional; `None` means "no filter on this dimension".
/// Results are ordered newest-first.
#[derive(Debug, Default)]
pub struct QualityGateFilter {
    /// Restrict to rows whose `skill` equals this value.
    pub skill: Option<String>,
    /// Restrict to rows whose `result` equals this value.
    pub result: Option<GateResult>,
    /// Restrict to rows whose `branch` equals this value.
    pub branch: Option<String>,
    /// Restrict to rows whose `created_at` is >= this RFC3339 timestamp.
    pub since: Option<String>,
}

/// Aggregate stats for a single skill across matching rows.
#[derive(Debug, Clone, serde::Serialize)]
pub struct QualityGateStats {
    pub skill: String,
    /// Total number of gate runs recorded for this skill.
    pub runs: u64,
    /// Runs whose result was "clean".
    pub clean: u64,
    /// Runs whose result was "issues".
    pub issues: u64,
    /// Fraction of runs that found issues: `issues / runs`.
    /// Zero when `runs == 0` (unreachable in practice since the row
    /// only appears when there is at least one run).
    pub catch_rate: f64,
    /// Sum of `findings_count` across all matching rows.
    pub total_findings: u64,
    /// Maximum `findings_count` across all matching rows.
    pub max_findings: u64,
}

/// Join predicate fragments into a SQL `WHERE` clause, combining them with
/// `AND`, or the empty string when there are no predicates.
fn where_and(predicates: &[&str]) -> String {
    if predicates.is_empty() {
        String::new()
    } else {
        format!("WHERE {}", predicates.join(" AND "))
    }
}

impl Database {
    /// Return gate rows matching `filter`, newest first.
    ///
    /// An empty result set is not an error; the caller decides how to
    /// present it. All filter fields are applied with AND semantics.
    pub fn list_quality_gates(&self, filter: &QualityGateFilter) -> Result<Vec<QualityGateRow>> {
        // Build (predicate, param) pairs together so a SQL fragment and its
        // bound value can never drift out of order.
        let mut clauses: Vec<(&str, Box<dyn rusqlite::ToSql>)> = Vec::new();
        if let Some(ref s) = filter.skill {
            clauses.push(("skill = ?", Box::new(s.clone())));
        }
        if let Some(ref r) = filter.result {
            clauses.push(("result = ?", Box::new(r.as_str().to_owned())));
        }
        if let Some(ref b) = filter.branch {
            clauses.push(("branch = ?", Box::new(b.clone())));
        }
        if let Some(ref ts) = filter.since {
            clauses.push(("created_at >= ?", Box::new(ts.clone())));
        }

        let predicates: Vec<&str> = clauses.iter().map(|(p, _)| *p).collect();
        let where_clause = where_and(&predicates);

        let sql = format!(
            "SELECT id, branch, commit_hash, skill, result, findings_count, details, created_at \
             FROM quality_gates \
             {where_clause} \
             ORDER BY created_at DESC"
        );

        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(
            rusqlite::params_from_iter(clauses.iter().map(|(_, v)| v.as_ref())),
            |row| {
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
            },
        )?;

        let mut out: Vec<QualityGateRow> = Vec::new();
        for row in rows {
            out.push(row.map_err(LegionError::Database)?);
        }
        Ok(out)
    }

    /// Return per-skill aggregate statistics, optionally filtered by `skill`
    /// and/or `since`.
    ///
    /// The aggregation runs in SQLite (SUM/COUNT/MAX), then `catch_rate` is
    /// computed in Rust from the returned counts so no floating-point SQL
    /// functions are needed. Results are ordered by skill name ascending.
    pub fn quality_gate_stats(
        &self,
        skill: Option<&str>,
        since: Option<&str>,
    ) -> Result<Vec<QualityGateStats>> {
        let mut clauses: Vec<(&str, Box<dyn rusqlite::ToSql>)> = Vec::new();
        if let Some(s) = skill {
            clauses.push(("skill = ?", Box::new(s.to_owned())));
        }
        if let Some(ts) = since {
            clauses.push(("created_at >= ?", Box::new(ts.to_owned())));
        }

        let predicates: Vec<&str> = clauses.iter().map(|(p, _)| *p).collect();
        let where_clause = where_and(&predicates);

        let sql = format!(
            "SELECT \
               skill, \
               COUNT(*) AS runs, \
               SUM(CASE WHEN result = 'clean'  THEN 1 ELSE 0 END) AS clean, \
               SUM(CASE WHEN result = 'issues' THEN 1 ELSE 0 END) AS issues, \
               SUM(findings_count) AS total_findings, \
               MAX(findings_count) AS max_findings \
             FROM quality_gates \
             {where_clause} \
             GROUP BY skill \
             ORDER BY skill ASC"
        );

        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(
            rusqlite::params_from_iter(clauses.iter().map(|(_, v)| v.as_ref())),
            |row| {
                let skill_str: String = row.get(0)?;
                let runs_i64: i64 = row.get(1)?;
                let clean_i64: i64 = row.get(2)?;
                let issues_i64: i64 = row.get(3)?;
                let total_i64: i64 = row.get(4)?;
                let max_i64: i64 = row.get(5)?;
                Ok((
                    skill_str, runs_i64, clean_i64, issues_i64, total_i64, max_i64,
                ))
            },
        )?;

        let mut out: Vec<QualityGateStats> = Vec::new();
        for row in rows {
            let (skill_str, runs_i64, clean_i64, issues_i64, total_i64, max_i64) =
                row.map_err(LegionError::Database)?;
            let runs = runs_i64.unsigned_abs();
            let clean = clean_i64.unsigned_abs();
            let issues = issues_i64.unsigned_abs();
            let catch_rate = if runs == 0 {
                0.0
            } else {
                issues as f64 / runs as f64
            };
            out.push(QualityGateStats {
                skill: skill_str,
                runs,
                clean,
                issues,
                catch_rate,
                total_findings: total_i64.unsigned_abs(),
                max_findings: max_i64.unsigned_abs(),
            });
        }
        Ok(out)
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

    // --- list_quality_gates tests ---

    #[test]
    fn list_quality_gates_empty_returns_empty_vec() {
        use crate::db::quality_gates::QualityGateFilter;
        let db = test_db();
        let rows = db
            .list_quality_gates(&QualityGateFilter::default())
            .unwrap();
        assert!(rows.is_empty());
    }

    #[test]
    fn list_quality_gates_newest_first() {
        use crate::db::quality_gates::QualityGateFilter;
        let db = test_db();
        db.record_quality_gate(
            "main",
            "hash-a",
            "legion-simplify",
            GateResult::Clean,
            0,
            None,
        )
        .unwrap();
        // Force a strictly later timestamp so ORDER BY created_at DESC is
        // deterministic; two back-to-back inserts can otherwise land in the
        // same sub-second RFC3339 bucket (same fix as the filter_by_since test).
        std::thread::sleep(std::time::Duration::from_millis(1));
        db.record_quality_gate(
            "main",
            "hash-b",
            "legion-simplify",
            GateResult::Issues,
            2,
            None,
        )
        .unwrap();

        let rows = db
            .list_quality_gates(&QualityGateFilter::default())
            .unwrap();
        assert_eq!(rows.len(), 2);
        // Second insert is more recent; it must appear first.
        assert_eq!(rows[0].commit_hash, "hash-b");
        assert_eq!(rows[1].commit_hash, "hash-a");
    }

    #[test]
    fn list_quality_gates_filter_by_skill() {
        use crate::db::quality_gates::QualityGateFilter;
        let db = test_db();
        db.record_quality_gate("main", "h1", "legion-simplify", GateResult::Clean, 0, None)
            .unwrap();
        db.record_quality_gate("main", "h2", "legion-review", GateResult::Issues, 1, None)
            .unwrap();

        let rows = db
            .list_quality_gates(&QualityGateFilter {
                skill: Some("legion-simplify".to_owned()),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].skill, "legion-simplify");
    }

    #[test]
    fn list_quality_gates_filter_by_result() {
        use crate::db::quality_gates::QualityGateFilter;
        let db = test_db();
        db.record_quality_gate("main", "h1", "legion-simplify", GateResult::Clean, 0, None)
            .unwrap();
        db.record_quality_gate("main", "h2", "legion-simplify", GateResult::Issues, 3, None)
            .unwrap();
        db.record_quality_gate("main", "h3", "legion-review", GateResult::Clean, 0, None)
            .unwrap();

        let rows = db
            .list_quality_gates(&QualityGateFilter {
                result: Some(GateResult::Issues),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].result, GateResult::Issues);
        assert_eq!(rows[0].commit_hash, "h2");
    }

    #[test]
    fn list_quality_gates_filter_by_branch() {
        use crate::db::quality_gates::QualityGateFilter;
        let db = test_db();
        db.record_quality_gate("feat/foo", "h1", "s", GateResult::Clean, 0, None)
            .unwrap();
        db.record_quality_gate("main", "h2", "s", GateResult::Clean, 0, None)
            .unwrap();

        let rows = db
            .list_quality_gates(&QualityGateFilter {
                branch: Some("feat/foo".to_owned()),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].branch, "feat/foo");
    }

    #[test]
    fn list_quality_gates_filter_by_since() {
        use crate::db::quality_gates::QualityGateFilter;
        let db = test_db();
        // Insert two rows then filter by a timestamp that falls between them.
        // We control created_at by inserting rows with a known sleep order;
        // since the DB stores RFC3339 strings and sorts lexicographically we can
        // insert rows and capture their timestamps to build the filter.
        let row_a = db
            .record_quality_gate("main", "h-old", "s", GateResult::Clean, 0, None)
            .unwrap();
        // Sleep briefly so the second row has a strictly later timestamp.
        std::thread::sleep(std::time::Duration::from_millis(10));
        let row_b = db
            .record_quality_gate("main", "h-new", "s", GateResult::Issues, 1, None)
            .unwrap();

        // Filter with since = row_b.created_at -- only row_b should match.
        let rows = db
            .list_quality_gates(&QualityGateFilter {
                since: Some(row_b.created_at.clone()),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(rows.len(), 1, "expected only the newer row");
        assert_eq!(rows[0].commit_hash, "h-new");
        // row_a is before the cutoff.
        assert!(rows.iter().all(|r| r.commit_hash != row_a.commit_hash));
    }

    // --- quality_gate_stats tests ---

    #[test]
    fn quality_gate_stats_empty_returns_empty_vec() {
        let db = test_db();
        let stats = db.quality_gate_stats(None, None).unwrap();
        assert!(stats.is_empty());
    }

    #[test]
    fn quality_gate_stats_counts_and_catch_rate() {
        let db = test_db();
        // 3 runs for legion-simplify: 1 clean, 2 issues.
        db.record_quality_gate("main", "h1", "legion-simplify", GateResult::Clean, 0, None)
            .unwrap();
        db.record_quality_gate("main", "h2", "legion-simplify", GateResult::Issues, 5, None)
            .unwrap();
        db.record_quality_gate("main", "h3", "legion-simplify", GateResult::Issues, 3, None)
            .unwrap();
        // 1 run for legion-review: 1 clean, findings = 0.
        db.record_quality_gate("main", "h4", "legion-review", GateResult::Clean, 0, None)
            .unwrap();

        let stats = db.quality_gate_stats(None, None).unwrap();
        assert_eq!(stats.len(), 2, "expected two skill rows");

        // Results are skill-ordered ASC: legion-review first.
        let review = &stats[0];
        assert_eq!(review.skill, "legion-review");
        assert_eq!(review.runs, 1);
        assert_eq!(review.clean, 1);
        assert_eq!(review.issues, 0);
        assert!((review.catch_rate - 0.0).abs() < f64::EPSILON);
        assert_eq!(review.total_findings, 0);
        assert_eq!(review.max_findings, 0);

        let simplify = &stats[1];
        assert_eq!(simplify.skill, "legion-simplify");
        assert_eq!(simplify.runs, 3);
        assert_eq!(simplify.clean, 1);
        assert_eq!(simplify.issues, 2);
        // catch_rate = issues / runs = 2/3
        let expected_rate = 2.0_f64 / 3.0_f64;
        assert!(
            (simplify.catch_rate - expected_rate).abs() < 1e-10,
            "catch_rate mismatch: {} vs {}",
            simplify.catch_rate,
            expected_rate
        );
        assert_eq!(simplify.total_findings, 8); // 0 + 5 + 3
        assert_eq!(simplify.max_findings, 5);
    }

    #[test]
    fn quality_gate_stats_filter_by_skill() {
        let db = test_db();
        db.record_quality_gate("main", "h1", "legion-simplify", GateResult::Issues, 2, None)
            .unwrap();
        db.record_quality_gate("main", "h2", "legion-review", GateResult::Clean, 0, None)
            .unwrap();

        let stats = db
            .quality_gate_stats(Some("legion-simplify"), None)
            .unwrap();
        assert_eq!(stats.len(), 1);
        assert_eq!(stats[0].skill, "legion-simplify");
        assert_eq!(stats[0].issues, 1);
    }

    #[test]
    fn quality_gate_stats_catch_rate_all_clean() {
        let db = test_db();
        db.record_quality_gate("main", "h1", "s", GateResult::Clean, 0, None)
            .unwrap();
        db.record_quality_gate("main", "h2", "s", GateResult::Clean, 0, None)
            .unwrap();

        let stats = db.quality_gate_stats(None, None).unwrap();
        assert_eq!(stats.len(), 1);
        assert!((stats[0].catch_rate - 0.0).abs() < f64::EPSILON);
    }

    #[test]
    fn quality_gate_stats_catch_rate_all_issues() {
        let db = test_db();
        db.record_quality_gate("main", "h1", "s", GateResult::Issues, 1, None)
            .unwrap();
        db.record_quality_gate("main", "h2", "s", GateResult::Issues, 1, None)
            .unwrap();

        let stats = db.quality_gate_stats(None, None).unwrap();
        assert_eq!(stats.len(), 1);
        assert!((stats[0].catch_rate - 1.0).abs() < f64::EPSILON);
    }
}
