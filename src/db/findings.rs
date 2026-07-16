//! Quality-gate findings ledger (#773): a `quality_gate_findings` row per
//! structured finding a gate run (`legion-simplify`, `legion-review`)
//! surfaced, keyed to the `quality_gates.id` row that recorded it. Owns the
//! `quality_gate_findings` DDL.
//!
//! Before this table, a finding existed only as free text inside
//! `quality_gates.details` -- once a gate was recorded "clean" (or even
//! "issues"), the individual finding was unrecoverable except by re-reading
//! that JSON blob by hand, and nothing forced it to ever be acted on. This
//! table makes a finding a first-class row with a lifecycle: PENDING until a
//! later commit demonstrably touches the flagged file (RESOLVED, detected by
//! `crate::finding_gate::reconcile_pending_findings`) or an operator/agent
//! explicitly says why it will not be fixed (DISPOSITIONED, via
//! `dispose_finding` / `batch_ack_low_findings`). `crate::finding_gate`
//! reads the PENDING set to decide whether a `clean` verdict may be
//! recorded at all.

use chrono::Utc;
use rusqlite::Connection;
use uuid::Uuid;

use super::Database;
use crate::error::{LegionError, Result};
use crate::finding_gate::{FindingSeverity, FindingStatus};

/// `quality_gate_findings` table (#773). New table -- no `migrate()` is
/// needed yet; future column additions here follow the has_column-ALTER
/// pattern the other domain files use (see `quality_gates::migrate`).
pub(super) fn create_tables(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS quality_gate_findings (
                id TEXT PRIMARY KEY,
                gate_id TEXT NOT NULL,
                branch TEXT NOT NULL,
                skill TEXT NOT NULL,
                origin_commit TEXT NOT NULL,
                file TEXT NOT NULL,
                line INTEGER,
                severity TEXT NOT NULL,
                summary TEXT NOT NULL,
                status TEXT NOT NULL DEFAULT 'pending',
                disposition_reason TEXT,
                resolved_by_commit TEXT,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_quality_gate_findings_gate
                ON quality_gate_findings(gate_id);
            CREATE INDEX IF NOT EXISTS idx_quality_gate_findings_branch_skill_status
                ON quality_gate_findings(branch, skill, status);",
    )?;
    Ok(())
}

/// One structured finding extracted from a quality-gate run, keyed to the
/// `quality_gates.id` row that recorded it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct QualityGateFinding {
    pub id: String,
    /// `quality_gates.id` of the gate row this finding was extracted from.
    pub gate_id: String,
    /// Denormalized from the gate row so the pending-set query (branch +
    /// skill, #773's refusal predicate) never needs a join.
    pub branch: String,
    pub skill: String,
    /// `quality_gates.commit_hash` of the gate row this finding was raised
    /// on -- the commit resolution detection reconciles *from*.
    pub origin_commit: String,
    pub file: String,
    pub line: Option<i64>,
    pub severity: FindingSeverity,
    pub summary: String,
    pub status: FindingStatus,
    /// Required alongside a DISPOSITIONED status -- a disposition with no
    /// reason is not an audit trail (mirrors `quality_gates.void_reason`).
    pub disposition_reason: Option<String>,
    /// The commit that resolved this finding, set only when `status` is
    /// RESOLVED (see `crate::finding_gate::reconcile_pending_findings`).
    pub resolved_by_commit: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

/// Input for inserting a new finding (#773).
pub struct NewFindingInput<'a> {
    pub gate_id: &'a str,
    pub branch: &'a str,
    pub skill: &'a str,
    pub origin_commit: &'a str,
    pub file: &'a str,
    pub line: Option<i64>,
    pub severity: FindingSeverity,
    pub summary: &'a str,
}

/// Filter parameters for `list_findings`, the audit surface (#773 AC4). All
/// fields are optional; `None` means "no filter on this dimension".
#[derive(Debug, Default)]
pub struct FindingFilter {
    pub branch: Option<String>,
    pub skill: Option<String>,
    pub status: Option<FindingStatus>,
}

fn parse_severity_from_db(s: String) -> std::result::Result<FindingSeverity, rusqlite::Error> {
    s.parse().map_err(|e: LegionError| {
        rusqlite::Error::FromSqlConversionFailure(
            8,
            rusqlite::types::Type::Text,
            Box::new(std::io::Error::other(e.to_string())),
        )
    })
}

fn parse_status_from_db(s: String) -> std::result::Result<FindingStatus, rusqlite::Error> {
    s.parse().map_err(|e: LegionError| {
        rusqlite::Error::FromSqlConversionFailure(
            9,
            rusqlite::types::Type::Text,
            Box::new(std::io::Error::other(e.to_string())),
        )
    })
}

/// Shared row-mapping closure for the 13-column `SELECT ... FROM
/// quality_gate_findings` shape every read path here uses.
fn row_to_finding(
    row: &rusqlite::Row<'_>,
) -> std::result::Result<QualityGateFinding, rusqlite::Error> {
    let severity_str: String = row.get(7)?;
    let status_str: String = row.get(9)?;
    Ok(QualityGateFinding {
        id: row.get(0)?,
        gate_id: row.get(1)?,
        branch: row.get(2)?,
        skill: row.get(3)?,
        origin_commit: row.get(4)?,
        file: row.get(5)?,
        line: row.get(6)?,
        severity: parse_severity_from_db(severity_str)?,
        summary: row.get(8)?,
        status: parse_status_from_db(status_str)?,
        disposition_reason: row.get(10)?,
        resolved_by_commit: row.get(11)?,
        created_at: row.get(12)?,
        updated_at: row.get(13)?,
    })
}

const SELECT_COLUMNS: &str = "id, gate_id, branch, skill, origin_commit, file, line, severity, \
     summary, status, disposition_reason, resolved_by_commit, created_at, updated_at";

impl Database {
    /// Insert a new PENDING finding extracted from a just-recorded gate row.
    pub fn insert_finding(&self, input: &NewFindingInput<'_>) -> Result<QualityGateFinding> {
        let id = Uuid::now_v7().to_string();
        let now = Utc::now().to_rfc3339();
        self.conn.execute(
            "INSERT INTO quality_gate_findings \
             (id, gate_id, branch, skill, origin_commit, file, line, severity, summary, \
              status, disposition_reason, resolved_by_commit, created_at, updated_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, NULL, NULL, ?11, ?11)",
            rusqlite::params![
                &id,
                input.gate_id,
                input.branch,
                input.skill,
                input.origin_commit,
                input.file,
                input.line,
                input.severity.as_str(),
                input.summary,
                FindingStatus::Pending.as_str(),
                &now,
            ],
        )?;
        Ok(QualityGateFinding {
            id,
            gate_id: input.gate_id.to_owned(),
            branch: input.branch.to_owned(),
            skill: input.skill.to_owned(),
            origin_commit: input.origin_commit.to_owned(),
            file: input.file.to_owned(),
            line: input.line,
            severity: input.severity,
            summary: input.summary.to_owned(),
            status: FindingStatus::Pending,
            disposition_reason: None,
            resolved_by_commit: None,
            created_at: now.clone(),
            updated_at: now,
        })
    }

    /// Look up a finding by id regardless of status, or `None` if it does not exist.
    pub fn get_finding_by_id(&self, id: &str) -> Result<Option<QualityGateFinding>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {SELECT_COLUMNS} FROM quality_gate_findings WHERE id = ?1"
        ))?;
        let mut rows = stmt.query_map(rusqlite::params![id], row_to_finding)?;
        match rows.next() {
            Some(Ok(row)) => Ok(Some(row)),
            Some(Err(e)) => Err(LegionError::Database(e)),
            None => Ok(None),
        }
    }

    /// PENDING findings for a (branch, skill) pair, oldest first -- the set
    /// `crate::finding_gate::evaluate_refusal` and
    /// `crate::finding_gate::reconcile_pending_findings` both read.
    pub fn list_pending_findings(
        &self,
        branch: &str,
        skill: &str,
    ) -> Result<Vec<QualityGateFinding>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {SELECT_COLUMNS} FROM quality_gate_findings \
             WHERE branch = ?1 AND skill = ?2 AND status = ?3 \
             ORDER BY created_at ASC"
        ))?;
        let rows = stmt.query_map(
            rusqlite::params![branch, skill, FindingStatus::Pending.as_str()],
            row_to_finding,
        )?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row.map_err(LegionError::Database)?);
        }
        Ok(out)
    }

    /// The audit surface (#773 AC4): every finding matching `filter`, newest
    /// first, across every status -- fixed (RESOLVED), waived
    /// (DISPOSITIONED), and PENDING alike.
    pub fn list_findings(&self, filter: &FindingFilter) -> Result<Vec<QualityGateFinding>> {
        let mut clauses: Vec<(&str, Box<dyn rusqlite::ToSql>)> = Vec::new();
        if let Some(ref b) = filter.branch {
            clauses.push(("branch = ?", Box::new(b.clone())));
        }
        if let Some(ref s) = filter.skill {
            clauses.push(("skill = ?", Box::new(s.clone())));
        }
        if let Some(s) = filter.status {
            clauses.push(("status = ?", Box::new(s.as_str().to_owned())));
        }
        let predicates: Vec<&str> = clauses.iter().map(|(p, _)| *p).collect();
        let where_clause = if predicates.is_empty() {
            String::new()
        } else {
            format!("WHERE {}", predicates.join(" AND "))
        };
        let sql = format!(
            "SELECT {SELECT_COLUMNS} FROM quality_gate_findings {where_clause} \
             ORDER BY created_at DESC"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(
            rusqlite::params_from_iter(clauses.iter().map(|(_, v)| v.as_ref())),
            row_to_finding,
        )?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row.map_err(LegionError::Database)?);
        }
        Ok(out)
    }

    /// Mark a PENDING finding DISPOSITIONED with an explicit reason (#773 --
    /// "silent drop is refused"). Errors when the finding does not exist, or
    /// when it is not currently PENDING (a RESOLVED finding needs no
    /// disposition; re-dispositioning an already-DISPOSITIONED one should go
    /// through the same explicit id-scoped call, which this still permits --
    /// only a RESOLVED finding is refused, since silently overriding proof of
    /// a fix with a waiver would be the exact hole this table closes).
    pub fn dispose_finding(&self, id: &str, reason: &str) -> Result<QualityGateFinding> {
        let existing = self
            .get_finding_by_id(id)?
            .ok_or_else(|| LegionError::FindingNotFound(id.to_owned()))?;
        if existing.status == FindingStatus::Resolved {
            return Err(LegionError::FindingAlreadyResolved(id.to_owned()));
        }
        let now = Utc::now().to_rfc3339();
        self.conn.execute(
            "UPDATE quality_gate_findings \
             SET status = ?1, disposition_reason = ?2, updated_at = ?3 \
             WHERE id = ?4",
            rusqlite::params![FindingStatus::Dispositioned.as_str(), reason, &now, id],
        )?;
        self.get_finding_by_id(id)?
            .ok_or_else(|| LegionError::FindingNotFound(id.to_owned()))
    }

    /// Mark a finding RESOLVED with the commit that resolved it. Used only by
    /// `crate::finding_gate::reconcile_pending_findings` (git-log detection),
    /// never directly by a CLI verb -- a resolution is discovered, not
    /// asserted.
    pub fn mark_finding_resolved(
        &self,
        id: &str,
        resolved_by_commit: &str,
    ) -> Result<QualityGateFinding> {
        let now = Utc::now().to_rfc3339();
        let affected = self.conn.execute(
            "UPDATE quality_gate_findings \
             SET status = ?1, resolved_by_commit = ?2, updated_at = ?3 \
             WHERE id = ?4",
            rusqlite::params![
                FindingStatus::Resolved.as_str(),
                resolved_by_commit,
                &now,
                id
            ],
        )?;
        if affected == 0 {
            return Err(LegionError::FindingNotFound(id.to_owned()));
        }
        self.get_finding_by_id(id)?
            .ok_or_else(|| LegionError::FindingNotFound(id.to_owned()))
    }

    /// Batch-acknowledge every PENDING LOW-severity finding for a
    /// (branch, skill) pair with one shared reason (#773 AC3 -- "a conscious
    /// sweep, not per-nit ceremony"). Each finding is still dispositioned
    /// individually (its own row, its own `updated_at`), so the audit trail
    /// stays per-finding even though the reason is shared. Returns the
    /// findings that were acknowledged; an empty vec is not an error (there
    /// may simply be nothing pending to ack).
    pub fn batch_ack_low_findings(
        &self,
        branch: &str,
        skill: &str,
        reason: &str,
    ) -> Result<Vec<QualityGateFinding>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {SELECT_COLUMNS} FROM quality_gate_findings \
             WHERE branch = ?1 AND skill = ?2 AND status = ?3 AND severity = ?4 \
             ORDER BY created_at ASC"
        ))?;
        let rows = stmt.query_map(
            rusqlite::params![
                branch,
                skill,
                FindingStatus::Pending.as_str(),
                FindingSeverity::Low.as_str()
            ],
            row_to_finding,
        )?;
        let mut targets = Vec::new();
        for row in rows {
            targets.push(row.map_err(LegionError::Database)?);
        }
        let mut acked = Vec::with_capacity(targets.len());
        for finding in &targets {
            acked.push(self.dispose_finding(&finding.id, reason)?);
        }
        Ok(acked)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::testutil::test_db;

    fn input<'a>(
        gate_id: &'a str,
        file: &'a str,
        severity: FindingSeverity,
    ) -> NewFindingInput<'a> {
        NewFindingInput {
            gate_id,
            branch: "feat/x",
            skill: "legion-simplify",
            origin_commit: "commit-a",
            file,
            line: Some(42),
            severity,
            summary: "duplicate logic in two match arms",
        }
    }

    #[test]
    fn insert_and_lookup_finding_by_id() {
        let db = test_db();
        let row = db
            .insert_finding(&input("gate-1", "src/foo.rs", FindingSeverity::Med))
            .unwrap();
        assert!(!row.id.is_empty());
        assert_eq!(row.status, FindingStatus::Pending);
        assert!(row.disposition_reason.is_none());
        assert!(row.resolved_by_commit.is_none());

        let fetched = db.get_finding_by_id(&row.id).unwrap().unwrap();
        assert_eq!(fetched.file, "src/foo.rs");
        assert_eq!(fetched.severity, FindingSeverity::Med);
    }

    #[test]
    fn get_finding_by_id_missing_returns_none() {
        let db = test_db();
        assert!(db.get_finding_by_id("no-such-id").unwrap().is_none());
    }

    #[test]
    fn list_pending_findings_scoped_to_branch_and_skill() {
        let db = test_db();
        db.insert_finding(&input("gate-1", "src/a.rs", FindingSeverity::High))
            .unwrap();
        let mut other_skill = input("gate-2", "src/b.rs", FindingSeverity::High);
        other_skill.skill = "legion-review";
        db.insert_finding(&other_skill).unwrap();
        let mut other_branch = input("gate-3", "src/c.rs", FindingSeverity::High);
        other_branch.branch = "feat/other";
        db.insert_finding(&other_branch).unwrap();

        let pending = db
            .list_pending_findings("feat/x", "legion-simplify")
            .unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].file, "src/a.rs");
    }

    #[test]
    fn dispose_finding_sets_status_and_reason() {
        let db = test_db();
        let row = db
            .insert_finding(&input("gate-1", "src/foo.rs", FindingSeverity::Low))
            .unwrap();
        let disposed = db
            .dispose_finding(&row.id, "won't fix: intentional")
            .unwrap();
        assert_eq!(disposed.status, FindingStatus::Dispositioned);
        assert_eq!(
            disposed.disposition_reason.as_deref(),
            Some("won't fix: intentional")
        );
    }

    #[test]
    fn dispose_finding_missing_id_errors() {
        let db = test_db();
        let err = db.dispose_finding("no-such-id", "reason").unwrap_err();
        assert!(err.to_string().contains("no-such-id"));
    }

    #[test]
    fn dispose_finding_refuses_an_already_resolved_row() {
        let db = test_db();
        let row = db
            .insert_finding(&input("gate-1", "src/foo.rs", FindingSeverity::Med))
            .unwrap();
        db.mark_finding_resolved(&row.id, "commit-b").unwrap();
        let err = db.dispose_finding(&row.id, "reason").unwrap_err();
        assert!(err.to_string().contains(&row.id));
    }

    #[test]
    fn mark_finding_resolved_sets_status_and_commit() {
        let db = test_db();
        let row = db
            .insert_finding(&input("gate-1", "src/foo.rs", FindingSeverity::High))
            .unwrap();
        let resolved = db.mark_finding_resolved(&row.id, "commit-fix").unwrap();
        assert_eq!(resolved.status, FindingStatus::Resolved);
        assert_eq!(resolved.resolved_by_commit.as_deref(), Some("commit-fix"));
    }

    #[test]
    fn mark_finding_resolved_missing_id_errors() {
        let db = test_db();
        let err = db
            .mark_finding_resolved("no-such-id", "commit-x")
            .unwrap_err();
        assert!(err.to_string().contains("no-such-id"));
    }

    #[test]
    fn resolved_finding_excluded_from_pending_list() {
        let db = test_db();
        let row = db
            .insert_finding(&input("gate-1", "src/foo.rs", FindingSeverity::High))
            .unwrap();
        db.mark_finding_resolved(&row.id, "commit-fix").unwrap();
        let pending = db
            .list_pending_findings("feat/x", "legion-simplify")
            .unwrap();
        assert!(pending.is_empty());
    }

    #[test]
    fn batch_ack_low_findings_only_touches_pending_low_severity() {
        let db = test_db();
        db.insert_finding(&input("gate-1", "src/low-a.rs", FindingSeverity::Low))
            .unwrap();
        db.insert_finding(&input("gate-1", "src/low-b.rs", FindingSeverity::Low))
            .unwrap();
        db.insert_finding(&input("gate-1", "src/high.rs", FindingSeverity::High))
            .unwrap();

        let acked = db
            .batch_ack_low_findings("feat/x", "legion-simplify", "batch ack: formatting only")
            .unwrap();
        assert_eq!(acked.len(), 2);
        assert!(
            acked
                .iter()
                .all(|f| f.disposition_reason.as_deref() == Some("batch ack: formatting only"))
        );

        let pending = db
            .list_pending_findings("feat/x", "legion-simplify")
            .unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].severity, FindingSeverity::High);
    }

    #[test]
    fn batch_ack_low_findings_empty_when_nothing_pending() {
        let db = test_db();
        let acked = db
            .batch_ack_low_findings("feat/x", "legion-simplify", "reason")
            .unwrap();
        assert!(acked.is_empty());
    }

    #[test]
    fn list_findings_filters_by_status() {
        let db = test_db();
        let a = db
            .insert_finding(&input("gate-1", "src/a.rs", FindingSeverity::High))
            .unwrap();
        db.insert_finding(&input("gate-1", "src/b.rs", FindingSeverity::Med))
            .unwrap();
        db.dispose_finding(&a.id, "won't fix").unwrap();

        let dispositioned = db
            .list_findings(&FindingFilter {
                status: Some(FindingStatus::Dispositioned),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(dispositioned.len(), 1);
        assert_eq!(dispositioned[0].file, "src/a.rs");

        let pending = db
            .list_findings(&FindingFilter {
                status: Some(FindingStatus::Pending),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].file, "src/b.rs");
    }

    #[test]
    fn list_findings_empty_returns_empty_vec() {
        let db = test_db();
        let rows = db.list_findings(&FindingFilter::default()).unwrap();
        assert!(rows.is_empty());
    }
}
