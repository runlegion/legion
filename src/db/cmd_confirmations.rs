//! legion-cmd confirmations (#1237, FR-CMD-026): the one-time confirmations
//! `legion cmd confirm` records and the PreToolUse adapter reads and uses up.
//! Owns the `cmd_confirmations` DDL. The table is local to this node: it is
//! not on the sync wire, since a confirmation is bound to one session.

use chrono::{DateTime, SecondsFormat, Utc};
use rusqlite::Connection;

use super::Database;
use crate::error::Result;

/// `cmd_confirmations` table.
pub(super) fn create_tables(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS cmd_confirmations (
                id TEXT PRIMARY KEY,
                session_id TEXT NOT NULL,
                command_key TEXT NOT NULL,
                reason TEXT NOT NULL,
                recorded_at TEXT NOT NULL,
                used_at TEXT
            );
            CREATE INDEX IF NOT EXISTS idx_cmd_confirmations_session
                ON cmd_confirmations(session_id, command_key)
                WHERE used_at IS NULL;",
    )?;
    Ok(())
}

/// One stored confirmation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CmdConfirmation {
    /// UUIDv7; the confirmation's incident record carries the same id.
    pub id: String,
    pub session_id: String,
    /// `legion_cmd::CommandKey::as_str` of the confirmed command.
    pub command_key: String,
    pub reason: String,
    pub recorded_at: DateTime<Utc>,
    pub used_at: Option<DateTime<Utc>>,
}

/// One fixed-width timestamp format, so the `recorded_at` range checks can
/// compare stored text directly.
fn fmt_ts(ts: DateTime<Utc>) -> String {
    ts.to_rfc3339_opts(SecondsFormat::Micros, true)
}

fn parse_ts(raw: &str) -> rusqlite::Result<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(raw)
        .map(|ts| ts.with_timezone(&Utc))
        .map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
        })
}

fn map_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<CmdConfirmation> {
    let recorded_at: String = row.get(4)?;
    let used_at: Option<String> = row.get(5)?;
    Ok(CmdConfirmation {
        id: row.get(0)?,
        session_id: row.get(1)?,
        command_key: row.get(2)?,
        reason: row.get(3)?,
        recorded_at: parse_ts(&recorded_at)?,
        used_at: used_at.as_deref().map(parse_ts).transpose()?,
    })
}

const COLUMNS: &str = "id, session_id, command_key, reason, recorded_at, used_at";

impl Database {
    /// Stores one confirmation.
    pub fn insert_cmd_confirmation(&self, confirmation: &CmdConfirmation) -> Result<()> {
        self.conn.execute(
            "INSERT INTO cmd_confirmations \
             (id, session_id, command_key, reason, recorded_at, used_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![
                confirmation.id,
                confirmation.session_id,
                confirmation.command_key,
                confirmation.reason,
                fmt_ts(confirmation.recorded_at),
                confirmation.used_at.map(fmt_ts),
            ],
        )?;
        Ok(())
    }

    /// This session's unused confirmations recorded at or after `since`,
    /// oldest first.
    pub fn live_cmd_confirmations(
        &self,
        session_id: &str,
        since: DateTime<Utc>,
    ) -> Result<Vec<CmdConfirmation>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {COLUMNS} FROM cmd_confirmations \
             WHERE session_id = ?1 AND used_at IS NULL AND recorded_at >= ?2 \
             ORDER BY recorded_at ASC"
        ))?;
        let rows = stmt
            .query_map(rusqlite::params![session_id, fmt_ts(since)], map_row)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// One confirmation by id.
    pub fn cmd_confirmation(&self, id: &str) -> Result<Option<CmdConfirmation>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {COLUMNS} FROM cmd_confirmations WHERE id = ?1"
        ))?;
        let mut rows = stmt.query_map(rusqlite::params![id], map_row)?;
        Ok(rows.next().transpose()?)
    }

    /// Uses up the oldest unused confirmation for `command_key` in this
    /// session recorded at or after `since`. Returns false when none was left
    /// to use.
    pub fn use_cmd_confirmation(
        &self,
        session_id: &str,
        command_key: &str,
        since: DateTime<Utc>,
        now: DateTime<Utc>,
    ) -> Result<bool> {
        let changed = self.conn.execute(
            "UPDATE cmd_confirmations SET used_at = ?4 WHERE id = (\
                 SELECT id FROM cmd_confirmations \
                 WHERE session_id = ?1 AND command_key = ?2 AND used_at IS NULL \
                   AND recorded_at >= ?3 \
                 ORDER BY recorded_at ASC LIMIT 1)",
            rusqlite::params![session_id, command_key, fmt_ts(since), fmt_ts(now)],
        )?;
        Ok(changed == 1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::testutil::test_db;
    use chrono::Duration;

    fn confirmation(id: &str, session: &str, key: &str, at: DateTime<Utc>) -> CmdConfirmation {
        CmdConfirmation {
            id: id.to_string(),
            session_id: session.to_string(),
            command_key: key.to_string(),
            reason: "because".to_string(),
            recorded_at: at,
            used_at: None,
        }
    }

    #[test]
    fn live_confirmations_are_scoped_to_the_session_the_window_and_unused_rows() {
        let db = test_db();
        let now = Utc::now();
        db.insert_cmd_confirmation(&confirmation("a", "s1", "k", now))
            .expect("insert");
        db.insert_cmd_confirmation(&confirmation("b", "s2", "k", now))
            .expect("insert");
        db.insert_cmd_confirmation(&confirmation("c", "s1", "k", now - Duration::minutes(11)))
            .expect("insert");

        let since = now - Duration::minutes(10);
        let live = db.live_cmd_confirmations("s1", since).expect("read");
        assert_eq!(live.len(), 1);
        assert_eq!(live[0].id, "a");

        assert!(db.use_cmd_confirmation("s1", "k", since, now).expect("use"));
        assert!(
            db.live_cmd_confirmations("s1", since)
                .expect("read")
                .is_empty()
        );
        // Used up: a second use finds nothing.
        assert!(!db.use_cmd_confirmation("s1", "k", since, now).expect("use"));
        let used = db.cmd_confirmation("a").expect("read").expect("row");
        assert!(used.used_at.is_some());
    }
}
