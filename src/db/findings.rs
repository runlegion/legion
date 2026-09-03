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
    /// Also carries the gate row's void reason when status is VOIDED (#1126,
    /// `void_findings_by_gate_tx`) -- reusing this column rather than adding a
    /// new one, since both cases are "why this finding stopped being
    /// PENDING, stated explicitly".
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
            7,
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

/// Shared row-mapping closure for the 14-column `SELECT ... FROM
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

/// Escape SQL `LIKE` metacharacters in a user-supplied id prefix (#840).
///
/// The prefix paths exist precisely for HAND-TYPED input, and `_` matches
/// any single character while `%` matches any run. A typo containing `_`
/// that happened to match exactly one row would resolve and then be
/// dispositioned or voided -- a state change on a row nobody named. A
/// genuine copied id is unaffected: UUIDs are hex and dashes only.
///
/// Paired with `ESCAPE '\'` in every query that consumes this. The
/// backslash itself is escaped first so it cannot smuggle in an escape.
pub(super) fn escape_like_prefix(prefix: &str) -> String {
    prefix
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

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

    /// Every finding whose id STARTS WITH `prefix`, ordered by id (#840).
    ///
    /// Anchored with `LIKE ?1 || '%'`, never a substring search: a prefix is
    /// what a human copies off the front of a printed id, so matching mid-id
    /// would resolve ids nobody typed. Returns whole rows rather than ids
    /// because the caller's ambiguity error has to name `file:line` -- ids
    /// alone do not disambiguate findings minted in the same millisecond,
    /// which share their leading 24 characters.
    pub fn find_findings_by_id_prefix(&self, prefix: &str) -> Result<Vec<QualityGateFinding>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {SELECT_COLUMNS} FROM quality_gate_findings \
             WHERE id LIKE ?1 || '%' ESCAPE '\\' ORDER BY id"
        ))?;
        let rows = stmt.query_map(
            rusqlite::params![escape_like_prefix(prefix)],
            row_to_finding,
        )?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row.map_err(LegionError::Database)?);
        }
        Ok(out)
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
    /// when it is already in a terminal state -- RESOLVED (a later commit
    /// demonstrably fixed it) or VOIDED (its gate run was declared
    /// not-evidence, #1126) -- since re-dispositioning either would
    /// overwrite `disposition_reason` with a fabricated "someone judged this
    /// and waived it" story, clobbering the proof of what actually happened.
    /// Re-dispositioning an already-DISPOSITIONED finding is still permitted
    /// through this same explicit id-scoped call -- only the two terminal
    /// states are refused, each with its own error
    /// (`LegionError::FindingAlreadyResolved` /
    /// `LegionError::FindingAlreadyVoided`) so the message names which one
    /// actually blocked the call rather than collapsing both into one lie.
    ///
    /// `id` is matched EXACTLY. Prefix acceptance is a CLI-layer convenience
    /// (`crate::cli::verify::resolve_finding_id`) deliberately kept out of
    /// here, so the internal callers that already hold a full id --
    /// `batch_ack_low_findings` and `finding_gate::reconcile_pending_findings`
    /// -- cannot start resolving fuzzily by accident (#840).
    pub fn dispose_finding(&self, id: &str, reason: &str) -> Result<QualityGateFinding> {
        let existing = self
            .get_finding_by_id(id)?
            .ok_or_else(|| LegionError::FindingNotFound(id.to_owned()))?;
        match existing.status {
            FindingStatus::Resolved => {
                return Err(LegionError::FindingAlreadyResolved(id.to_owned()));
            }
            FindingStatus::Voided => {
                return Err(LegionError::FindingAlreadyVoided(id.to_owned()));
            }
            FindingStatus::Pending | FindingStatus::Dispositioned => {}
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

    /// Void every PENDING finding raised by `gate_id`'s run (#1126): the gate
    /// row itself was declared not-evidence (e.g. recorded from a worktree
    /// parked on the wrong branch/HEAD), so the findings it raised were never
    /// weighed on their merits and must stop counting toward the PENDING set
    /// `evaluate_refusal`/`list_pending_findings` read. Only PENDING findings
    /// are touched -- a finding already RESOLVED (genuinely fixed) or
    /// DISPOSITIONED (genuinely judged) already has a truthful terminal
    /// status of its own, and overwriting it with VOIDED would erase that
    /// history rather than add to it.
    ///
    /// Returns the number of findings voided. Zero is not an error -- a gate
    /// row with no findings, or one whose findings already left PENDING
    /// before the void, is a legitimate no-op (#1126 error-handling
    /// requirement).
    ///
    /// Takes the CALLER's already-open transaction rather than opening its
    /// own (#1126 review MED, mirroring `wake::set_wake_attempt_work_item_tx`'s
    /// precedent for the same shape). This is the ONLY entry point -- there
    /// is deliberately no standalone `&self`-taking wrapper that opens its
    /// own transaction, because every real caller needs this cascade
    /// committed together with the `quality_gates` row void it follows:
    /// `Database::void_quality_gate` opens one transaction, does the
    /// gate-row UPDATE, then calls this with that same transaction before
    /// committing. A failure partway through (e.g. SQLite BUSY from a
    /// concurrent writer -- the watch daemon holds the same database file --
    /// is genuinely reachable here) rolls back both writes instead of
    /// leaving the gate row voided with its findings still PENDING.
    pub(crate) fn void_findings_by_gate_tx(
        tx: &rusqlite::Transaction,
        gate_id: &str,
        reason: &str,
    ) -> Result<usize> {
        let now = Utc::now().to_rfc3339();
        let affected = tx.execute(
            "UPDATE quality_gate_findings \
             SET status = ?1, disposition_reason = ?2, updated_at = ?3 \
             WHERE gate_id = ?4 AND status = ?5",
            rusqlite::params![
                FindingStatus::Voided.as_str(),
                reason,
                &now,
                gate_id,
                FindingStatus::Pending.as_str(),
            ],
        )?;
        Ok(affected)
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

    // --- #840: the prefix read path the CLI resolver is built on ----------

    #[test]
    fn find_findings_by_id_prefix_returns_every_match_in_id_order() {
        let db = test_db();
        let a = db
            .insert_finding(&input("gate-1", "src/foo.rs", FindingSeverity::Med))
            .unwrap();
        let b = db
            .insert_finding(&input("gate-1", "src/bar.rs", FindingSeverity::Med))
            .unwrap();
        // Derive the shared prefix from the ids themselves rather than
        // assuming 8 characters: two inserts straddling a timestamp
        // rollover would share fewer, and the assumption would flake.
        let prefix: String =
            a.id.chars()
                .zip(b.id.chars())
                .take_while(|(x, y)| x == y)
                .map(|(x, _)| x)
                .collect();
        assert!(
            !prefix.is_empty(),
            "back-to-back UUIDv7 ids must share a leading timestamp run"
        );

        let matches = db.find_findings_by_id_prefix(&prefix).unwrap();
        assert_eq!(matches.len(), 2, "both findings must come back");
        let mut expected = vec![a.id, b.id];
        expected.sort();
        assert_eq!(
            matches.iter().map(|f| f.id.clone()).collect::<Vec<_>>(),
            expected
        );
        // Whole rows, not bare ids -- the caller's ambiguity error needs
        // file:line to be choosable.
        assert!(matches.iter().any(|f| f.file == "src/foo.rs"));
        assert!(matches.iter().any(|f| f.file == "src/bar.rs"));
    }

    /// #840 review finding: these prefix paths exist for HAND-TYPED input,
    /// and an unescaped `_` matches any single character under LIKE. A typo
    /// that happened to match exactly one row would resolve and then be
    /// dispositioned -- a state change on a row nobody named.
    #[test]
    fn id_prefix_treats_like_wildcards_as_literal_characters() {
        let db = test_db();
        let a = db
            .insert_finding(&input("gate-1", "src/foo.rs", FindingSeverity::Med))
            .unwrap();

        // `_` as a wildcard would match the real id at that position; as a
        // literal it matches nothing, because UUIDs carry no underscores.
        let mut wild: String = a.id.chars().take(7).collect();
        wild.push('_');
        assert!(
            db.find_findings_by_id_prefix(&wild).unwrap().is_empty(),
            "underscore must be literal, not a single-character wildcard"
        );

        // `%` likewise: as a wildcard it would match every row.
        assert!(
            db.find_findings_by_id_prefix("%").unwrap().is_empty(),
            "percent must be literal, not a match-everything wildcard"
        );

        // The honest prefix still resolves, so the escaping did not break
        // the path it protects.
        let real: String = a.id.chars().take(8).collect();
        assert_eq!(db.find_findings_by_id_prefix(&real).unwrap().len(), 1);
    }

    #[test]
    fn find_findings_by_id_prefix_is_anchored_not_a_substring_search() {
        let db = test_db();
        let row = db
            .insert_finding(&input("gate-1", "src/foo.rs", FindingSeverity::Med))
            .unwrap();
        let middle: String = row.id.chars().skip(4).take(8).collect();
        assert!(
            db.find_findings_by_id_prefix(&middle).unwrap().is_empty(),
            "a mid-id fragment must not match; only a leading prefix does"
        );
        assert!(
            db.find_findings_by_id_prefix("no-such-id")
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn dispose_finding_matches_the_id_exactly() {
        // Prefix acceptance is the CLI resolver's job (see
        // `cli::verify::resolve_finding_id`); this layer stays exact so the
        // internal callers holding full ids cannot resolve fuzzily.
        let db = test_db();
        let row = db
            .insert_finding(&input("gate-1", "src/foo.rs", FindingSeverity::Med))
            .unwrap();

        let prefix: String = row.id.chars().take(8).collect();
        let err = db.dispose_finding(&prefix, "x").unwrap_err();
        assert!(
            matches!(err, LegionError::FindingNotFound(_)),
            "expected FindingNotFound for a prefix, got {err:?}"
        );

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

    /// #1126 review MED1: dispositioning a VOIDED finding must be refused,
    /// not silently overwrite `disposition_reason` -- which was holding the
    /// void reason -- with a fabricated waiver story. Distinct from
    /// `dispose_finding_refuses_an_already_resolved_row`: a voided finding
    /// must error with a message naming VOIDED specifically (not "already
    /// resolved", which would itself be a false claim about what happened
    /// to this finding).
    #[test]
    fn dispose_finding_refuses_a_voided_row() {
        let db = test_db();
        let row = db
            .insert_finding(&input("gate-1", "src/foo.rs", FindingSeverity::Med))
            .unwrap();
        void_via_tx(&db, "gate-1", "recorded against the wrong branch");
        let before = db.get_finding_by_id(&row.id).unwrap().unwrap();
        assert_eq!(before.status, FindingStatus::Voided);

        let err = db
            .dispose_finding(&row.id, "won't fix: intentional")
            .unwrap_err();
        assert!(err.to_string().contains(&row.id));
        assert!(
            err.to_string().contains("VOIDED"),
            "expected the error to name VOIDED specifically, got: {err}"
        );

        // Not just an error return -- the row itself must be untouched: the
        // void reason must survive, not be clobbered by the attempted
        // disposition reason.
        let after = db.get_finding_by_id(&row.id).unwrap().unwrap();
        assert_eq!(after.status, FindingStatus::Voided);
        assert_eq!(
            after.disposition_reason.as_deref(),
            Some("recorded against the wrong branch"),
            "the void reason must survive a refused disposition attempt"
        );
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

    /// `branch` and `skill` filters combine with AND semantics, not just
    /// each in isolation -- a row matching only one of the two must not
    /// appear when both are supplied.
    #[test]
    fn list_findings_filters_by_branch_and_skill_combined() {
        let db = test_db();
        // Matches both filters.
        db.insert_finding(&input("gate-1", "src/match.rs", FindingSeverity::High))
            .unwrap();
        // Matches branch only (different skill).
        let mut other_skill = input("gate-2", "src/other-skill.rs", FindingSeverity::High);
        other_skill.skill = "legion-review";
        db.insert_finding(&other_skill).unwrap();
        // Matches skill only (different branch).
        let mut other_branch = input("gate-3", "src/other-branch.rs", FindingSeverity::High);
        other_branch.branch = "feat/other";
        db.insert_finding(&other_branch).unwrap();
        // Matches neither.
        let mut neither = input("gate-4", "src/neither.rs", FindingSeverity::High);
        neither.branch = "feat/other";
        neither.skill = "legion-review";
        db.insert_finding(&neither).unwrap();

        let rows = db
            .list_findings(&FindingFilter {
                branch: Some("feat/x".to_string()),
                skill: Some("legion-simplify".to_string()),
                status: None,
            })
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].file, "src/match.rs");
    }

    #[test]
    fn list_findings_empty_returns_empty_vec() {
        let db = test_db();
        let rows = db.list_findings(&FindingFilter::default()).unwrap();
        assert!(rows.is_empty());
    }

    // --- void_findings_by_gate_tx (#1126) ---

    /// Test-only convenience: `void_findings_by_gate_tx` takes the caller's
    /// already-open transaction (by design -- see its doc comment, #1126
    /// review MED), so every test here opens one, calls it, and commits,
    /// standing in for what `Database::void_quality_gate` does in
    /// production.
    fn void_via_tx(db: &Database, gate_id: &str, reason: &str) -> usize {
        let tx = db.conn.unchecked_transaction().unwrap();
        let affected = Database::void_findings_by_gate_tx(&tx, gate_id, reason).unwrap();
        tx.commit().unwrap();
        affected
    }

    #[test]
    fn void_findings_by_gate_marks_pending_findings_voided_with_reason() {
        let db = test_db();
        let a = db
            .insert_finding(&input("gate-1", "src/a.rs", FindingSeverity::High))
            .unwrap();
        let b = db
            .insert_finding(&input("gate-1", "src/b.rs", FindingSeverity::Low))
            .unwrap();

        let count = void_via_tx(&db, "gate-1", "recorded against the wrong branch");
        assert_eq!(count, 2);

        for id in [&a.id, &b.id] {
            let refetched = db.get_finding_by_id(id).unwrap().unwrap();
            assert_eq!(refetched.status, FindingStatus::Voided);
            assert_eq!(
                refetched.disposition_reason.as_deref(),
                Some("recorded against the wrong branch")
            );
        }
    }

    #[test]
    fn void_findings_by_gate_no_findings_is_a_no_op_not_an_error() {
        let db = test_db();
        let count = void_via_tx(&db, "no-such-gate", "reason");
        assert_eq!(count, 0);
    }

    /// A voided finding must stop counting toward the pending set that
    /// refuses a `clean` verdict (#1126's core requirement).
    #[test]
    fn voided_finding_excluded_from_pending_list() {
        let db = test_db();
        db.insert_finding(&input("gate-1", "src/a.rs", FindingSeverity::High))
            .unwrap();

        void_via_tx(&db, "gate-1", "wrong branch");

        let pending = db
            .list_pending_findings("feat/x", "legion-simplify")
            .unwrap();
        assert!(pending.is_empty());
    }

    /// Voiding one gate's run must not touch findings from a DIFFERENT,
    /// un-voided run -- a fix that stops findings blocking in general would
    /// be worse than the bug (#1126).
    #[test]
    fn void_findings_by_gate_does_not_touch_other_gates_findings() {
        let db = test_db();
        db.insert_finding(&input("gate-voided", "src/a.rs", FindingSeverity::High))
            .unwrap();
        let other = db
            .insert_finding(&input("gate-live", "src/b.rs", FindingSeverity::High))
            .unwrap();

        void_via_tx(&db, "gate-voided", "wrong branch");

        let refetched_other = db.get_finding_by_id(&other.id).unwrap().unwrap();
        assert_eq!(
            refetched_other.status,
            FindingStatus::Pending,
            "a finding from a different, un-voided gate run must still block"
        );

        let pending = db
            .list_pending_findings("feat/x", "legion-simplify")
            .unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].id, other.id);
    }

    /// A finding already RESOLVED or DISPOSITIONED before its gate was voided
    /// keeps its own truthful terminal status -- voiding must not overwrite
    /// genuine history with VOIDED.
    #[test]
    fn void_findings_by_gate_does_not_overwrite_resolved_or_dispositioned() {
        let db = test_db();
        let resolved = db
            .insert_finding(&input("gate-1", "src/resolved.rs", FindingSeverity::High))
            .unwrap();
        db.mark_finding_resolved(&resolved.id, "commit-fix")
            .unwrap();
        let dispositioned = db
            .insert_finding(&input("gate-1", "src/waived.rs", FindingSeverity::Med))
            .unwrap();
        db.dispose_finding(&dispositioned.id, "won't fix").unwrap();
        let pending = db
            .insert_finding(&input("gate-1", "src/pending.rs", FindingSeverity::Low))
            .unwrap();

        let count = void_via_tx(&db, "gate-1", "wrong branch");
        assert_eq!(count, 1, "only the still-PENDING finding is voided");

        assert_eq!(
            db.get_finding_by_id(&resolved.id).unwrap().unwrap().status,
            FindingStatus::Resolved
        );
        assert_eq!(
            db.get_finding_by_id(&dispositioned.id)
                .unwrap()
                .unwrap()
                .status,
            FindingStatus::Dispositioned
        );
        assert_eq!(
            db.get_finding_by_id(&pending.id).unwrap().unwrap().status,
            FindingStatus::Voided
        );
    }

    /// `list_findings` orders newest first (matches `list_quality_gates`'s
    /// own newest-first convention) -- the audit surface (#773 AC4) reads
    /// top-down as "most recent first", not insertion order.
    #[test]
    fn list_findings_newest_first() {
        let db = test_db();
        db.insert_finding(&input("gate-1", "src/older.rs", FindingSeverity::High))
            .unwrap();
        // Force a strictly later timestamp, same technique
        // `quality_gates.rs`'s own `list_quality_gates_newest_first` test
        // uses for the same sub-second RFC3339 bucket collision risk.
        std::thread::sleep(std::time::Duration::from_millis(1));
        db.insert_finding(&input("gate-2", "src/newer.rs", FindingSeverity::High))
            .unwrap();

        let rows = db.list_findings(&FindingFilter::default()).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(
            rows[0].file, "src/newer.rs",
            "the newer row must sort first"
        );
        assert_eq!(rows[1].file, "src/older.rs");
    }
}
