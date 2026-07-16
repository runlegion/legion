//! Quality gates: skill-runner results keyed to commits so
//! `legion pr create` can verify a clean gate (#200). Owns the
//! `quality_gates` DDL.

use std::str::FromStr;

use chrono::Utc;
use rusqlite::Connection;
use uuid::Uuid;

use super::Database;
use crate::error::{LegionError, Result};
use crate::verify::{GateProvenance, GateResult};

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

/// Column migrations for `quality_gates`, in their original patch order.
///
/// The provenance/void/supersede block (#780) mirrors `tasks.deleted_at`
/// (src/db/kanban.rs migration 13) -- a known-false row is retired without
/// deleting history, and a re-laid genuine row can point back at what it
/// replaces. `provenance` defaults to `'asserted'` for pre-existing rows:
/// every row recorded before this migration went through the
/// un-validated `record` path (the `check` validator predates this column
/// but always calls `record_quality_gate` directly, so historical
/// `check`-recorded rows are indistinguishable from `record`-recorded ones
/// at the DB layer without a backfill this issue does not attempt) -- the
/// conservative default, since treating an unknown row as VALIDATED would
/// be exactly the ground-truth leak #780 closes.
///
/// The `base` column (#779) follows: `--base` override for `legion
/// quality-gate check`. Nullable -- existing rows and gates recorded
/// without an explicit `--base` (the default main/origin-main resolution,
/// or non-`check` gate kinds like `record` and `legion-verify`) have no
/// base to record. Recording the resolved base ref on the row (rather than
/// only inside the `details` JSON blob) keeps a too-narrow `--base`
/// visible to the same `quality-gate list`/`stats` surfaces that already
/// query columns, consistent with the issue's auditability-over-trust
/// stance.
pub(super) fn migrate(conn: &Connection) -> Result<()> {
    if !Database::has_column(conn, "quality_gates", "provenance")? {
        conn.execute_batch(
            "ALTER TABLE quality_gates ADD COLUMN provenance TEXT NOT NULL DEFAULT 'asserted';",
        )?;
    }
    if !Database::has_column(conn, "quality_gates", "voided_at")? {
        conn.execute_batch("ALTER TABLE quality_gates ADD COLUMN voided_at TEXT;")?;
    }
    if !Database::has_column(conn, "quality_gates", "void_reason")? {
        conn.execute_batch("ALTER TABLE quality_gates ADD COLUMN void_reason TEXT;")?;
    }
    if !Database::has_column(conn, "quality_gates", "superseded_by")? {
        conn.execute_batch("ALTER TABLE quality_gates ADD COLUMN superseded_by TEXT;")?;
    }
    if !Database::has_column(conn, "quality_gates", "base")? {
        conn.execute_batch("ALTER TABLE quality_gates ADD COLUMN base TEXT;")?;
    }
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
    /// Whether this verdict was structurally VALIDATED (`quality-gate
    /// check`) or merely ASSERTED (`quality-gate record`, #780).
    pub provenance: GateProvenance,
    /// RFC3339 timestamp this row was voided, if any. `None` means live.
    pub voided_at: Option<String>,
    /// Why the row was voided (required alongside `voided_at`; #780).
    pub void_reason: Option<String>,
    /// The id of the row that supersedes this one, if a re-laid genuine row
    /// has replaced it (#780).
    pub superseded_by: Option<String>,
    /// The `--base` ref the changed-file set was computed against (#779),
    /// when the caller supplied one. `None` for gates recorded without an
    /// explicit base (default main/origin-main resolution, or gate kinds
    /// that never compute a changed-file set, e.g. `legion-verify`).
    pub base: Option<String>,
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

/// Parse a `GateProvenance` from a DB string, mapping unknown values to a
/// `LegionError::Database` so the error stays in the rusqlite query path.
/// Column index 8 (`provenance`) in every SELECT below.
fn parse_provenance_from_db(s: String) -> std::result::Result<GateProvenance, rusqlite::Error> {
    GateProvenance::from_str(&s).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(
            8,
            rusqlite::types::Type::Text,
            Box::new(std::io::Error::other(e.to_string())),
        )
    })
}

/// Input for recording a quality gate result (#787).
///
/// Groups the positional argument list of `record_quality_gate` into a
/// single struct, following the `db::AuditInput` precedent, so future
/// additions (e.g. findings linkage) add a field instead of extending a
/// positional signature.
pub struct QualityGateInput<'a> {
    pub branch: &'a str,
    pub commit_hash: &'a str,
    pub skill: &'a str,
    pub result: GateResult,
    pub findings_count: u64,
    pub details: Option<&'a str>,
    /// Whether this verdict was VALIDATED (via `quality-gate check`) or
    /// merely ASSERTED (via `quality-gate record`, #780). The caller states
    /// this explicitly rather than it being inferred, so the write site that
    /// knows which code path it is (the `Check` handler vs. the `Record`
    /// handler) is the single source of truth for it.
    pub provenance: GateProvenance,
    /// The `--base` ref used to compute the changed-file set (#779), when
    /// the caller resolved one. `None` for gate kinds that never compute a
    /// changed-file set, or when the default main/origin-main resolution
    /// was used (as opposed to an explicit `--base` override).
    pub base: Option<&'a str>,
}

impl Database {
    /// Record a quality gate result for the given commit and skill.
    ///
    /// Multiple rows for the same (commit_hash, skill) pair are allowed --
    /// `get_quality_gate` returns the most recent one. This lets agents
    /// re-run the skill after fixing issues without losing the history.
    pub fn record_quality_gate(&self, input: &QualityGateInput<'_>) -> Result<QualityGateRow> {
        let id = Uuid::now_v7().to_string();
        let created_at = Utc::now().to_rfc3339();
        let result_str: &str = input.result.as_str();
        let provenance_str: &str = input.provenance.as_str();
        self.conn.execute(
            "INSERT INTO quality_gates \
             (id, branch, commit_hash, skill, result, findings_count, details, created_at, \
              provenance, base) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            rusqlite::params![
                &id,
                input.branch,
                input.commit_hash,
                input.skill,
                result_str,
                input.findings_count as i64,
                input.details,
                &created_at,
                provenance_str,
                input.base,
            ],
        )?;
        Ok(QualityGateRow {
            id,
            branch: input.branch.to_owned(),
            commit_hash: input.commit_hash.to_owned(),
            skill: input.skill.to_owned(),
            result: input.result,
            findings_count: input.findings_count,
            details: input.details.map(str::to_owned),
            created_at,
            provenance: input.provenance,
            voided_at: None,
            void_reason: None,
            superseded_by: None,
            base: input.base.map(str::to_owned),
        })
    }

    /// Return a gate row by its id regardless of live/voided state, or
    /// `None` if no row has that id. Used by `void_quality_gate` to return
    /// the post-update row, and available generally for id-keyed lookups
    /// (e.g. an operator auditing a `superseded_by` chain).
    pub fn get_quality_gate_by_id(&self, id: &str) -> Result<Option<QualityGateRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, branch, commit_hash, skill, result, findings_count, details, \
                    created_at, provenance, voided_at, void_reason, superseded_by, base \
             FROM quality_gates \
             WHERE id = ?1",
        )?;
        let mut rows = stmt.query_map(rusqlite::params![id], Self::row_to_gate)?;
        match rows.next() {
            Some(Ok(row)) => Ok(Some(row)),
            Some(Err(e)) => Err(LegionError::Database(e)),
            None => Ok(None),
        }
    }

    /// Void a gate row: mark it `voided_at`/`void_reason` so a known-false
    /// verdict is retired without deleting history (#780). Optionally names
    /// the row that supersedes it -- typically a re-laid genuine row from a
    /// fresh `quality-gate check` run, recorded separately and linked back
    /// here once its id is known.
    ///
    /// A voided row drops out of the "live" lookups (`get_quality_gate`,
    /// `get_latest_quality_gate_by_skill`) and out of `quality_gate_stats`,
    /// but stays visible in `list_quality_gates` and by direct id lookup, so
    /// the audit trail is never destroyed.
    ///
    /// Errors with `LegionError::QualityGateNotFound` when `id` does not
    /// match any row -- voiding a typo'd id must fail loudly, not silently
    /// no-op.
    pub fn void_quality_gate(
        &self,
        id: &str,
        reason: &str,
        superseded_by: Option<&str>,
    ) -> Result<QualityGateRow> {
        let now = Utc::now().to_rfc3339();
        let affected = self.conn.execute(
            "UPDATE quality_gates SET voided_at = ?1, void_reason = ?2, superseded_by = ?3 \
             WHERE id = ?4",
            rusqlite::params![&now, reason, superseded_by, id],
        )?;
        if affected == 0 {
            return Err(LegionError::QualityGateNotFound(id.to_owned()));
        }
        self.get_quality_gate_by_id(id)?
            .ok_or_else(|| LegionError::QualityGateNotFound(id.to_owned()))
    }

    /// Shared row-mapping closure for the 13-column `SELECT ... FROM
    /// quality_gates` shape every read path here uses, so a future column
    /// addition only changes the column list and this function once.
    fn row_to_gate(
        row: &rusqlite::Row<'_>,
    ) -> std::result::Result<QualityGateRow, rusqlite::Error> {
        let result_str: String = row.get(4)?;
        let findings_count_i64: i64 = row.get(5)?;
        let provenance_str: String = row.get(8)?;
        Ok(QualityGateRow {
            id: row.get(0)?,
            branch: row.get(1)?,
            commit_hash: row.get(2)?,
            skill: row.get(3)?,
            result: parse_result_from_db(result_str)?,
            findings_count: findings_count_i64.unsigned_abs(),
            details: row.get(6)?,
            created_at: row.get(7)?,
            provenance: parse_provenance_from_db(provenance_str)?,
            voided_at: row.get(9)?,
            void_reason: row.get(10)?,
            superseded_by: row.get(11)?,
            base: row.get(12)?,
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
        // Voided rows are excluded: a gate lookup this function feeds
        // (`pr create`'s pre-create check) must never treat a known-false
        // retired row as a live clean verdict (#780).
        let mut stmt = self.conn.prepare(
            "SELECT id, branch, commit_hash, skill, result, findings_count, details, \
                    created_at, provenance, voided_at, void_reason, superseded_by, base \
             FROM quality_gates \
             WHERE commit_hash = ?1 AND skill = ?2 AND voided_at IS NULL \
             ORDER BY created_at DESC \
             LIMIT 1",
        )?;
        let mut rows = stmt.query_map(rusqlite::params![commit_hash, skill], Self::row_to_gate)?;
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
        // Voided rows are excluded, same as `get_quality_gate` (#780): the
        // card-keyed ->Done check must never resolve to a retired verdict.
        let mut stmt = self.conn.prepare(
            "SELECT id, branch, commit_hash, skill, result, findings_count, details, \
                    created_at, provenance, voided_at, void_reason, superseded_by, base \
             FROM quality_gates \
             WHERE skill = ?1 AND voided_at IS NULL \
             ORDER BY created_at DESC \
             LIMIT 1",
        )?;
        let mut rows = stmt.query_map(rusqlite::params![skill], Self::row_to_gate)?;
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
    ///
    /// Unlike `get_quality_gate` / `get_latest_quality_gate_by_skill`, this
    /// does NOT exclude voided rows -- it is the audit surface, and a voided
    /// row (its `voided_at`/`void_reason`/`superseded_by` fields are part of
    /// `QualityGateRow`) must stay visible here so retiring a known-false
    /// row never reads as deleting history (#780).
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
            "SELECT id, branch, commit_hash, skill, result, findings_count, details, \
                    created_at, provenance, voided_at, void_reason, superseded_by, base \
             FROM quality_gates \
             {where_clause} \
             ORDER BY created_at DESC"
        );

        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(
            rusqlite::params_from_iter(clauses.iter().map(|(_, v)| v.as_ref())),
            Self::row_to_gate,
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
    /// Voided rows are excluded (#780): a known-false verdict must not
    /// inflate `clean`/`catch_rate` for the very skill it was retired for.
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

        let mut predicates: Vec<&str> = vec!["voided_at IS NULL"];
        predicates.extend(clauses.iter().map(|(p, _)| *p));
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
    use crate::db::Database;
    use crate::db::quality_gates::QualityGateInput;
    use crate::db::testutil::test_db;
    use crate::verify::{GateProvenance, GateResult};

    #[test]
    fn quality_gate_insert_and_lookup() {
        let db = test_db();
        let row = db
            .record_quality_gate(&QualityGateInput {
                branch: "feat/test-branch",
                commit_hash: "abc1234def5678",
                skill: "legion-simplify",
                result: GateResult::Clean,
                findings_count: 0,
                details: None,
                provenance: GateProvenance::Asserted,
                base: None,
            })
            .unwrap();
        assert!(!row.id.is_empty());
        assert_eq!(row.branch, "feat/test-branch");
        assert_eq!(row.commit_hash, "abc1234def5678");
        assert_eq!(row.skill, "legion-simplify");
        assert_eq!(row.result, GateResult::Clean);
        assert_eq!(row.findings_count, 0);
        assert!(row.details.is_none());
        assert_eq!(row.provenance, GateProvenance::Asserted);
        assert!(row.voided_at.is_none());
        assert!(row.void_reason.is_none());
        assert!(row.superseded_by.is_none());

        let fetched = db
            .get_quality_gate("abc1234def5678", "legion-simplify")
            .unwrap();
        assert!(fetched.is_some());
        let fetched = fetched.unwrap();
        assert_eq!(fetched.id, row.id);
        assert_eq!(fetched.result, GateResult::Clean);
        assert_eq!(fetched.provenance, GateProvenance::Asserted);
    }

    /// The `base` column (#779) round-trips through insert, direct lookup,
    /// and `list_quality_gates` -- the three read paths a gate row is
    /// visible through.
    #[test]
    fn quality_gate_base_column_round_trips_through_insert_and_reads() {
        use crate::db::quality_gates::QualityGateFilter;

        let db = test_db();
        let row = db
            .record_quality_gate(&QualityGateInput {
                branch: "feat/child",
                commit_hash: "base1234",
                skill: "legion-simplify",
                result: GateResult::Clean,
                findings_count: 0,
                details: None,
                provenance: GateProvenance::Asserted,
                base: Some("feat/base-branch"),
            })
            .unwrap();
        assert_eq!(row.base.as_deref(), Some("feat/base-branch"));

        let fetched = db
            .get_quality_gate("base1234", "legion-simplify")
            .unwrap()
            .expect("gate row should exist");
        assert_eq!(fetched.base.as_deref(), Some("feat/base-branch"));

        let listed = db
            .list_quality_gates(&QualityGateFilter {
                skill: Some("legion-simplify".to_owned()),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].base.as_deref(), Some("feat/base-branch"));
    }

    /// A gate recorded without an explicit base (the common case: `record`,
    /// `legion-verify`, or a `check` run against the default main/origin-main
    /// resolution's vacuous-initial-commit branch) stores `base` as `NULL`
    /// and reads back as `None`, not an empty string.
    #[test]
    fn quality_gate_base_column_defaults_to_none() {
        let db = test_db();
        let row = db
            .record_quality_gate(&QualityGateInput {
                branch: "main",
                commit_hash: "nobase123",
                skill: "legion-verify:card-1",
                result: GateResult::Clean,
                findings_count: 0,
                details: None,
                provenance: GateProvenance::Asserted,
                base: None,
            })
            .unwrap();
        assert!(row.base.is_none());

        let fetched = db
            .get_quality_gate("nobase123", "legion-verify:card-1")
            .unwrap()
            .expect("gate row should exist");
        assert!(fetched.base.is_none());
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
        db.record_quality_gate(&QualityGateInput {
            branch: "main",
            commit_hash: "abc1234",
            skill: "legion-simplify",
            result: GateResult::Clean,
            findings_count: 0,
            details: None,
            provenance: GateProvenance::Asserted,
            base: None,
        })
        .unwrap();
        // Different skill on the same commit should not match.
        let result = db.get_quality_gate("abc1234", "legion-review").unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn quality_gate_multiple_skills_on_same_commit() {
        let db = test_db();
        let hash = "deadbeef12345";
        db.record_quality_gate(&QualityGateInput {
            branch: "main",
            commit_hash: hash,
            skill: "legion-simplify",
            result: GateResult::Clean,
            findings_count: 0,
            details: None,
            provenance: GateProvenance::Asserted,
            base: None,
        })
        .unwrap();
        db.record_quality_gate(&QualityGateInput {
            branch: "main",
            commit_hash: hash,
            skill: "legion-review",
            result: GateResult::Issues,
            findings_count: 2,
            details: Some("{}"),
            provenance: GateProvenance::Asserted,
            base: None,
        })
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
        db.record_quality_gate(&QualityGateInput {
            branch: "main",
            commit_hash: hash,
            skill: "legion-simplify",
            result: GateResult::Issues,
            findings_count: 3,
            details: None,
            provenance: GateProvenance::Asserted,
            base: None,
        })
        .unwrap();
        // Second run after fixing: clean.
        db.record_quality_gate(&QualityGateInput {
            branch: "main",
            commit_hash: hash,
            skill: "legion-simplify",
            result: GateResult::Clean,
            findings_count: 0,
            details: None,
            provenance: GateProvenance::Asserted,
            base: None,
        })
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
            .record_quality_gate(&QualityGateInput {
                branch: "feat/x",
                commit_hash: "hash123",
                skill: "legion-simplify",
                result: GateResult::Issues,
                findings_count: 1,
                details: Some(details),
                provenance: GateProvenance::Asserted,
                base: None,
            })
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
        db.record_quality_gate(&QualityGateInput {
            branch: "feat/x",
            commit_hash: "commit-old",
            skill,
            result: GateResult::Issues,
            findings_count: 1,
            details: None,
            provenance: GateProvenance::Asserted,
            base: None,
        })
        .unwrap();
        db.record_quality_gate(&QualityGateInput {
            branch: "main",
            commit_hash: "commit-new",
            skill,
            result: GateResult::Clean,
            findings_count: 0,
            details: None,
            provenance: GateProvenance::Asserted,
            base: None,
        })
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
        db.record_quality_gate(&QualityGateInput {
            branch: "main",
            commit_hash: "hash-a",
            skill: "legion-simplify",
            result: GateResult::Clean,
            findings_count: 0,
            details: None,
            provenance: GateProvenance::Asserted,
            base: None,
        })
        .unwrap();
        // Force a strictly later timestamp so ORDER BY created_at DESC is
        // deterministic; two back-to-back inserts can otherwise land in the
        // same sub-second RFC3339 bucket (same fix as the filter_by_since test).
        std::thread::sleep(std::time::Duration::from_millis(1));
        db.record_quality_gate(&QualityGateInput {
            branch: "main",
            commit_hash: "hash-b",
            skill: "legion-simplify",
            result: GateResult::Issues,
            findings_count: 2,
            details: None,
            provenance: GateProvenance::Asserted,
            base: None,
        })
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
        db.record_quality_gate(&QualityGateInput {
            branch: "main",
            commit_hash: "h1",
            skill: "legion-simplify",
            result: GateResult::Clean,
            findings_count: 0,
            details: None,
            provenance: GateProvenance::Asserted,
            base: None,
        })
        .unwrap();
        db.record_quality_gate(&QualityGateInput {
            branch: "main",
            commit_hash: "h2",
            skill: "legion-review",
            result: GateResult::Issues,
            findings_count: 1,
            details: None,
            provenance: GateProvenance::Asserted,
            base: None,
        })
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
        db.record_quality_gate(&QualityGateInput {
            branch: "main",
            commit_hash: "h1",
            skill: "legion-simplify",
            result: GateResult::Clean,
            findings_count: 0,
            details: None,
            provenance: GateProvenance::Asserted,
            base: None,
        })
        .unwrap();
        db.record_quality_gate(&QualityGateInput {
            branch: "main",
            commit_hash: "h2",
            skill: "legion-simplify",
            result: GateResult::Issues,
            findings_count: 3,
            details: None,
            provenance: GateProvenance::Asserted,
            base: None,
        })
        .unwrap();
        db.record_quality_gate(&QualityGateInput {
            branch: "main",
            commit_hash: "h3",
            skill: "legion-review",
            result: GateResult::Clean,
            findings_count: 0,
            details: None,
            provenance: GateProvenance::Asserted,
            base: None,
        })
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
        db.record_quality_gate(&QualityGateInput {
            branch: "feat/foo",
            commit_hash: "h1",
            skill: "s",
            result: GateResult::Clean,
            findings_count: 0,
            details: None,
            provenance: GateProvenance::Asserted,
            base: None,
        })
        .unwrap();
        db.record_quality_gate(&QualityGateInput {
            branch: "main",
            commit_hash: "h2",
            skill: "s",
            result: GateResult::Clean,
            findings_count: 0,
            details: None,
            provenance: GateProvenance::Asserted,
            base: None,
        })
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
            .record_quality_gate(&QualityGateInput {
                branch: "main",
                commit_hash: "h-old",
                skill: "s",
                result: GateResult::Clean,
                findings_count: 0,
                details: None,
                provenance: GateProvenance::Asserted,
                base: None,
            })
            .unwrap();
        // Sleep briefly so the second row has a strictly later timestamp.
        std::thread::sleep(std::time::Duration::from_millis(10));
        let row_b = db
            .record_quality_gate(&QualityGateInput {
                branch: "main",
                commit_hash: "h-new",
                skill: "s",
                result: GateResult::Issues,
                findings_count: 1,
                details: None,
                provenance: GateProvenance::Asserted,
                base: None,
            })
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
        db.record_quality_gate(&QualityGateInput {
            branch: "main",
            commit_hash: "h1",
            skill: "legion-simplify",
            result: GateResult::Clean,
            findings_count: 0,
            details: None,
            provenance: GateProvenance::Asserted,
            base: None,
        })
        .unwrap();
        db.record_quality_gate(&QualityGateInput {
            branch: "main",
            commit_hash: "h2",
            skill: "legion-simplify",
            result: GateResult::Issues,
            findings_count: 5,
            details: None,
            provenance: GateProvenance::Asserted,
            base: None,
        })
        .unwrap();
        db.record_quality_gate(&QualityGateInput {
            branch: "main",
            commit_hash: "h3",
            skill: "legion-simplify",
            result: GateResult::Issues,
            findings_count: 3,
            details: None,
            provenance: GateProvenance::Asserted,
            base: None,
        })
        .unwrap();
        // 1 run for legion-review: 1 clean, findings = 0.
        db.record_quality_gate(&QualityGateInput {
            branch: "main",
            commit_hash: "h4",
            skill: "legion-review",
            result: GateResult::Clean,
            findings_count: 0,
            details: None,
            provenance: GateProvenance::Asserted,
            base: None,
        })
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
        db.record_quality_gate(&QualityGateInput {
            branch: "main",
            commit_hash: "h1",
            skill: "legion-simplify",
            result: GateResult::Issues,
            findings_count: 2,
            details: None,
            provenance: GateProvenance::Asserted,
            base: None,
        })
        .unwrap();
        db.record_quality_gate(&QualityGateInput {
            branch: "main",
            commit_hash: "h2",
            skill: "legion-review",
            result: GateResult::Clean,
            findings_count: 0,
            details: None,
            provenance: GateProvenance::Asserted,
            base: None,
        })
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
        db.record_quality_gate(&QualityGateInput {
            branch: "main",
            commit_hash: "h1",
            skill: "s",
            result: GateResult::Clean,
            findings_count: 0,
            details: None,
            provenance: GateProvenance::Asserted,
            base: None,
        })
        .unwrap();
        db.record_quality_gate(&QualityGateInput {
            branch: "main",
            commit_hash: "h2",
            skill: "s",
            result: GateResult::Clean,
            findings_count: 0,
            details: None,
            provenance: GateProvenance::Asserted,
            base: None,
        })
        .unwrap();

        let stats = db.quality_gate_stats(None, None).unwrap();
        assert_eq!(stats.len(), 1);
        assert!((stats[0].catch_rate - 0.0).abs() < f64::EPSILON);
    }

    #[test]
    fn quality_gate_stats_catch_rate_all_issues() {
        let db = test_db();
        db.record_quality_gate(&QualityGateInput {
            branch: "main",
            commit_hash: "h1",
            skill: "s",
            result: GateResult::Issues,
            findings_count: 1,
            details: None,
            provenance: GateProvenance::Asserted,
            base: None,
        })
        .unwrap();
        db.record_quality_gate(&QualityGateInput {
            branch: "main",
            commit_hash: "h2",
            skill: "s",
            result: GateResult::Issues,
            findings_count: 1,
            details: None,
            provenance: GateProvenance::Asserted,
            base: None,
        })
        .unwrap();

        let stats = db.quality_gate_stats(None, None).unwrap();
        assert_eq!(stats.len(), 1);
        assert!((stats[0].catch_rate - 1.0).abs() < f64::EPSILON);
    }

    // --- provenance + void/supersede tests (#780) ---

    #[test]
    fn provenance_validated_round_trips_through_lookup() {
        let db = test_db();
        db.record_quality_gate(&QualityGateInput {
            branch: "feat/x",
            commit_hash: "hash-validated",
            skill: "legion-simplify",
            result: GateResult::Clean,
            findings_count: 0,
            details: None,
            provenance: GateProvenance::Validated,
            base: None,
        })
        .unwrap();

        let fetched = db
            .get_quality_gate("hash-validated", "legion-simplify")
            .unwrap()
            .expect("row should exist");
        assert_eq!(fetched.provenance, GateProvenance::Validated);
    }

    #[test]
    fn void_quality_gate_sets_voided_fields_and_reason() {
        let db = test_db();
        let row = db
            .record_quality_gate(&QualityGateInput {
                branch: "feat/1754-alert-port",
                commit_hash: "e74a06d6",
                skill: "legion-simplify",
                result: GateResult::Clean,
                findings_count: 0,
                details: None,
                provenance: GateProvenance::Asserted,
                base: None,
            })
            .unwrap();

        let voided = db
            .void_quality_gate(
                &row.id,
                "manufactured clean, no articulation ever ran",
                None,
            )
            .unwrap();
        assert_eq!(voided.id, row.id);
        assert!(voided.voided_at.is_some());
        assert_eq!(
            voided.void_reason.as_deref(),
            Some("manufactured clean, no articulation ever ran")
        );
        assert!(voided.superseded_by.is_none());
    }

    #[test]
    fn void_quality_gate_can_link_a_superseding_row() {
        let db = test_db();
        let old = db
            .record_quality_gate(&QualityGateInput {
                branch: "feat/x",
                commit_hash: "hash-old",
                skill: "legion-simplify",
                result: GateResult::Clean,
                findings_count: 0,
                details: None,
                provenance: GateProvenance::Asserted,
                base: None,
            })
            .unwrap();
        let new = db
            .record_quality_gate(&QualityGateInput {
                branch: "feat/x",
                commit_hash: "hash-old",
                skill: "legion-simplify",
                result: GateResult::Clean,
                findings_count: 0,
                details: None,
                provenance: GateProvenance::Validated,
                base: None,
            })
            .unwrap();

        let voided = db
            .void_quality_gate(&old.id, "superseded by a validated re-run", Some(&new.id))
            .unwrap();
        assert_eq!(voided.superseded_by.as_deref(), Some(new.id.as_str()));
    }

    #[test]
    fn void_quality_gate_missing_id_errors() {
        let db = test_db();
        let err = db
            .void_quality_gate("no-such-id", "reason", None)
            .unwrap_err();
        assert!(err.to_string().contains("no-such-id"));
    }

    #[test]
    fn voided_row_excluded_from_get_quality_gate() {
        // A voided clean row must not satisfy a live lookup -- this is the
        // behavior that makes voiding actually retract the manufactured
        // clean (e.g. the disposed feat/1754-alert-port row) from `pr
        // create`'s gate check, not just annotate it.
        let db = test_db();
        let row = db
            .record_quality_gate(&QualityGateInput {
                branch: "feat/x",
                commit_hash: "hash-voided",
                skill: "legion-simplify",
                result: GateResult::Clean,
                findings_count: 0,
                details: None,
                provenance: GateProvenance::Asserted,
                base: None,
            })
            .unwrap();
        db.void_quality_gate(&row.id, "known false", None).unwrap();

        assert!(
            db.get_quality_gate("hash-voided", "legion-simplify")
                .unwrap()
                .is_none(),
            "a voided row must not be returned as the live gate"
        );
    }

    #[test]
    fn voided_row_excluded_from_latest_by_skill() {
        let db = test_db();
        let skill = "legion-verify:card-void";
        let row = db
            .record_quality_gate(&QualityGateInput {
                branch: "feat/x",
                commit_hash: "hash-a",
                skill,
                result: GateResult::Clean,
                findings_count: 0,
                details: None,
                provenance: GateProvenance::Asserted,
                base: None,
            })
            .unwrap();
        db.void_quality_gate(&row.id, "known false", None).unwrap();

        assert!(
            db.get_latest_quality_gate_by_skill(skill)
                .unwrap()
                .is_none(),
            "a voided row must not resolve as the latest live gate for the skill"
        );
    }

    #[test]
    fn voided_row_still_visible_in_list_quality_gates() {
        use crate::db::quality_gates::QualityGateFilter;
        let db = test_db();
        let row = db
            .record_quality_gate(&QualityGateInput {
                branch: "feat/x",
                commit_hash: "hash-listed",
                skill: "legion-simplify",
                result: GateResult::Clean,
                findings_count: 0,
                details: None,
                provenance: GateProvenance::Asserted,
                base: None,
            })
            .unwrap();
        db.void_quality_gate(&row.id, "known false", None).unwrap();

        let rows = db
            .list_quality_gates(&QualityGateFilter::default())
            .unwrap();
        assert_eq!(
            rows.len(),
            1,
            "voiding must not delete the row from history"
        );
        assert!(rows[0].voided_at.is_some());
        assert_eq!(rows[0].void_reason.as_deref(), Some("known false"));
    }

    #[test]
    fn voided_row_excluded_from_stats() {
        let db = test_db();
        let row = db
            .record_quality_gate(&QualityGateInput {
                branch: "main",
                commit_hash: "hash-stats-voided",
                skill: "legion-simplify",
                result: GateResult::Clean,
                findings_count: 0,
                details: None,
                provenance: GateProvenance::Asserted,
                base: None,
            })
            .unwrap();
        db.void_quality_gate(&row.id, "known false", None).unwrap();
        db.record_quality_gate(&QualityGateInput {
            branch: "main",
            commit_hash: "hash-stats-live",
            skill: "legion-simplify",
            result: GateResult::Issues,
            findings_count: 2,
            details: None,
            provenance: GateProvenance::Validated,
            base: None,
        })
        .unwrap();

        let stats = db.quality_gate_stats(None, None).unwrap();
        assert_eq!(stats.len(), 1);
        // Only the live row counts: 1 run, 0 clean, 1 issues -- the voided
        // clean row must not inflate `clean` or dilute `catch_rate`.
        assert_eq!(stats[0].runs, 1);
        assert_eq!(stats[0].clean, 0);
        assert_eq!(stats[0].issues, 1);
        assert!((stats[0].catch_rate - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn get_quality_gate_by_id_finds_a_voided_row_too() {
        // Unlike the live lookups, id-keyed lookup must still resolve a
        // voided row -- an operator auditing a supersede chain needs to walk
        // from the new row back to the one it replaced.
        let db = test_db();
        let row = db
            .record_quality_gate(&QualityGateInput {
                branch: "feat/x",
                commit_hash: "hash-by-id",
                skill: "legion-simplify",
                result: GateResult::Clean,
                findings_count: 0,
                details: None,
                provenance: GateProvenance::Asserted,
                base: None,
            })
            .unwrap();
        db.void_quality_gate(&row.id, "known false", None).unwrap();

        let fetched = db
            .get_quality_gate_by_id(&row.id)
            .unwrap()
            .expect("voided row must still be reachable by id");
        assert!(fetched.voided_at.is_some());
    }

    #[test]
    fn get_quality_gate_by_id_missing_returns_none() {
        let db = test_db();
        assert!(db.get_quality_gate_by_id("no-such-id").unwrap().is_none());
    }

    /// Migration is idempotent: opening the same file-backed database a
    /// second time must not fail even though the provenance/void columns
    /// (#780) AND the `base` column (#779) already exist. Mirrors
    /// kanban.rs's `migration_document_id_column_is_idempotent_on_reopen`.
    /// Both migrations share the same `migrate()` function and the same
    /// has_column guard mechanism, so one test recording both fields and
    /// reopening covers both double-apply paths.
    #[test]
    fn migration_provenance_void_and_base_columns_are_idempotent_on_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test_gate_migration.db");

        let db1 = Database::open(&db_path).expect("first open");
        let row = db1
            .record_quality_gate(&QualityGateInput {
                branch: "main",
                commit_hash: "reopen-hash",
                skill: "legion-simplify",
                result: GateResult::Clean,
                findings_count: 0,
                details: None,
                provenance: GateProvenance::Validated,
                base: Some("main"),
            })
            .expect("record on first open");
        assert_eq!(row.base.as_deref(), Some("main"));
        drop(db1);

        // Second open over the same path: both migrations' has_column
        // guards must prevent a duplicate ALTER TABLE from failing, and the
        // row written under the first open must still read back correctly.
        let db2 = Database::open(&db_path).expect("second open must not fail");
        let fetched = db2
            .get_quality_gate("reopen-hash", "legion-simplify")
            .expect("get")
            .expect("row from first open readable after second open");
        assert_eq!(fetched.id, row.id);
        assert_eq!(fetched.provenance, GateProvenance::Validated);
        assert_eq!(fetched.base.as_deref(), Some("main"));
        // dir is dropped here, cleaning up the temp files.
    }
}
