//! Audit log: work source action tracking (#142). Owns the `audit_log`
//! DDL.

use chrono::Utc;
use rusqlite::Connection;
use uuid::Uuid;

use super::Database;
use crate::error::Result;

/// `audit_log` table (#142).
pub(super) fn create_tables(conn: &Connection) -> Result<()> {
    // Migration 11: Audit log for work source actions (#142).
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS audit_log (
                id TEXT PRIMARY KEY,
                timestamp TEXT NOT NULL,
                agent TEXT NOT NULL,
                action TEXT NOT NULL,
                target_type TEXT NOT NULL,
                target_ref TEXT NOT NULL,
                task_id TEXT,
                source_type TEXT NOT NULL,
                details TEXT,
                outcome TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_audit_agent ON audit_log(agent);
            CREATE INDEX IF NOT EXISTS idx_audit_action ON audit_log(action);
            CREATE INDEX IF NOT EXISTS idx_audit_timestamp ON audit_log(timestamp);",
    )?;
    Ok(())
}

/// An entry in the audit log tracking work source actions.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct AuditEntry {
    pub id: String,
    pub timestamp: String,
    pub agent: String,
    pub action: String,
    pub target_type: String,
    pub target_ref: String,
    pub task_id: Option<String>,
    pub source_type: String,
    pub details: Option<String>,
    pub outcome: String,
}

/// Input for creating an audit log entry (no id/timestamp -- generated automatically).
pub struct AuditInput<'a> {
    pub agent: &'a str,
    pub action: &'a str,
    pub target_type: &'a str,
    pub target_ref: &'a str,
    pub task_id: Option<&'a str>,
    pub source_type: &'a str,
    pub details: Option<&'a str>,
    pub outcome: &'a str,
}

impl Database {
    /// Record an audit log entry for a work source action.
    pub fn insert_audit_entry(&self, input: &AuditInput<'_>) -> Result<String> {
        let id = Uuid::now_v7().to_string();
        let timestamp = Utc::now().to_rfc3339();
        self.conn.execute(
            "INSERT INTO audit_log (id, timestamp, agent, action, target_type, target_ref, task_id, source_type, details, outcome)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            rusqlite::params![
                id, timestamp, input.agent, input.action, input.target_type,
                input.target_ref, input.task_id, input.source_type, input.details, input.outcome
            ],
        )?;
        Ok(id)
    }

    /// Query audit log entries with optional filters.
    pub fn query_audit_log(
        &self,
        agent: Option<&str>,
        action: Option<&str>,
        limit: usize,
    ) -> Result<Vec<AuditEntry>> {
        let mut sql = String::from(
            "SELECT id, timestamp, agent, action, target_type, target_ref, task_id, source_type, details, outcome
             FROM audit_log WHERE 1=1",
        );
        let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();

        if let Some(a) = agent {
            sql.push_str(" AND agent = ?");
            params.push(Box::new(a.to_string()));
        }
        if let Some(a) = action {
            sql.push_str(" AND action = ?");
            params.push(Box::new(a.to_string()));
        }
        sql.push_str(" ORDER BY timestamp DESC LIMIT ?");
        params.push(Box::new(limit as i64));

        let param_refs: Vec<&dyn rusqlite::types::ToSql> =
            params.iter().map(|p| p.as_ref()).collect();
        let mut stmt = self.conn.prepare(&sql)?;
        let entries = stmt
            .query_map(param_refs.as_slice(), |row| {
                Ok(AuditEntry {
                    id: row.get(0)?,
                    timestamp: row.get(1)?,
                    agent: row.get(2)?,
                    action: row.get(3)?,
                    target_type: row.get(4)?,
                    target_ref: row.get(5)?,
                    task_id: row.get(6)?,
                    source_type: row.get(7)?,
                    details: row.get(8)?,
                    outcome: row.get(9)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;

        Ok(entries)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::db::testutil::test_db;

    #[test]
    fn audit_log_insert_and_query() {
        let db = test_db();
        let id = db
            .insert_audit_entry(&AuditInput {
                agent: "legion",
                action: "create-issue",
                target_type: "issue",
                target_ref: "42",
                task_id: None,
                source_type: "github",
                details: Some(r#"{"title":"test"}"#),
                outcome: "success",
            })
            .unwrap();
        assert!(!id.is_empty());

        let entries = db.query_audit_log(None, None, 10).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].agent, "legion");
        assert_eq!(entries[0].action, "create-issue");
        assert_eq!(entries[0].target_ref, "42");
        assert_eq!(entries[0].outcome, "success");
    }

    #[test]
    fn audit_log_filters_by_agent() {
        let db = test_db();
        db.insert_audit_entry(&AuditInput {
            agent: "legion",
            action: "create-issue",
            target_type: "issue",
            target_ref: "1",
            task_id: None,
            source_type: "github",
            details: None,
            outcome: "success",
        })
        .unwrap();
        db.insert_audit_entry(&AuditInput {
            agent: "rafters",
            action: "create-pr",
            target_type: "pr",
            target_ref: "2",
            task_id: None,
            source_type: "github",
            details: None,
            outcome: "success",
        })
        .unwrap();

        let legion_only = db.query_audit_log(Some("legion"), None, 10).unwrap();
        assert_eq!(legion_only.len(), 1);
        assert_eq!(legion_only[0].agent, "legion");

        let by_action = db.query_audit_log(None, Some("create-pr"), 10).unwrap();
        assert_eq!(by_action.len(), 1);
        assert_eq!(by_action[0].agent, "rafters");
    }

    #[test]
    fn audit_log_respects_limit() {
        let db = test_db();
        for i in 0..5 {
            db.insert_audit_entry(&AuditInput {
                agent: "legion",
                action: "comment",
                target_type: "comment",
                target_ref: &i.to_string(),
                task_id: None,
                source_type: "github",
                details: None,
                outcome: "success",
            })
            .unwrap();
        }
        let entries = db.query_audit_log(None, None, 3).unwrap();
        assert_eq!(entries.len(), 3);
    }
}
