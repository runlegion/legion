//! SQLite persistence layer, split by domain (#609).
//!
//! This module owns infrastructure only: the [`Database`] handle, `open`,
//! `has_column`, the `init_schema` dispatcher, and cross-table admin
//! (`rename_repo`). Every domain file carries its own `impl Database`
//! block plus the DDL and tests for its tables, so the public API is
//! identical to the old single-file `db.rs`.

mod audit;
mod autonomy;
mod board;
mod documents;
mod health;
mod heartbeat;
mod kanban;
mod quality_gates;
mod reflections;
mod schedules;
mod scip;
mod sessions;
mod stats;
mod statusline_samples;
mod sync;
#[cfg(test)]
pub(crate) mod testutil;
mod uncertainty;
mod wake;

pub use audit::AuditInput;
pub use kanban::CardTimestamp;
pub use reflections::{Reflection, ReflectionMeta};
pub use schedules::{Schedule, validate_hhmm};
pub use stats::DashboardRepoStats;

use std::path::Path;

use chrono::Utc;
use rusqlite::Connection;

use crate::error::{LegionError, Result};

/// Format an ISO 8601 timestamp to a date-only string (YYYY-MM-DD).
///
/// Falls back to the raw value if parsing fails, which keeps output
/// usable even with unexpected timestamp formats.
pub(crate) fn format_date(iso_timestamp: &str) -> &str {
    match iso_timestamp.split_once('T') {
        Some((date, _)) => date,
        None => iso_timestamp,
    }
}

/// Persistent storage for reflections backed by SQLite.
pub struct Database {
    pub(crate) conn: Connection,
}

impl Database {
    /// Open (or create) a SQLite database at the given path.
    ///
    /// Parent directories are created automatically if they do not exist.
    /// WAL mode is enabled for concurrent read performance.
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let conn = Connection::open(path)?;

        let mode: String = conn
            .pragma_query_value(None, "journal_mode", |row| row.get(0))
            .map_err(LegionError::Database)?;
        if mode != "wal" {
            conn.pragma_update(None, "journal_mode", "WAL")?;
        }

        Self::init_schema(&conn)?;

        Ok(Self { conn })
    }

    /// Check whether a table has a specific column via PRAGMA table_info.
    fn has_column(conn: &Connection, table: &str, column: &str) -> Result<bool> {
        let mut stmt = conn.prepare(&format!("PRAGMA table_info({})", table))?;
        let names: Vec<String> = stmt
            .query_map([], |row| {
                let name: String = row.get(1)?;
                Ok(name)
            })?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(LegionError::Database)?;
        Ok(names.iter().any(|n| n == column))
    }

    /// Create every table and index, then run the column migrations.
    ///
    /// Each domain file owns its DDL: the per-domain `create_tables`
    /// functions run the CREATE TABLE / CREATE INDEX statements for the
    /// base shape inside one transaction, and the `migrate` steps
    /// (has_column-guarded ALTERs, their backfills, and indexes over
    /// migrated columns) run after it, outside the transaction, in the
    /// same relative order they held in the single-file init_schema.
    fn init_schema(conn: &Connection) -> Result<()> {
        let tx = conn.unchecked_transaction()?;
        reflections::create_tables(conn)?;
        board::create_tables(conn)?;
        kanban::create_tables(conn)?;
        schedules::create_tables(conn)?;
        health::create_tables(conn)?;
        audit::create_tables(conn)?;
        quality_gates::create_tables(conn)?;
        statusline_samples::create_tables(conn)?;
        wake::create_tables(conn)?;
        scip::create_tables(conn)?;
        sessions::create_tables(conn)?;
        documents::create_tables(conn)?;
        uncertainty::create_tables(conn)?;
        autonomy::create_tables(conn)?;
        heartbeat::create_tables(conn)?;
        tx.commit()?;

        reflections::migrate(conn)?;
        board::migrate(conn)?;
        kanban::migrate(conn)?;
        schedules::migrate(conn)?;
        Ok(())
    }

    /// Rename a repo across all tables. Returns total rows updated.
    pub fn rename_repo(&self, from: &str, to: &str) -> Result<RenameCounts> {
        // unchecked_transaction because Database uses &self (shared ref),
        // but rusqlite::Connection::transaction() requires &mut self.
        // Safe here: no concurrent access within this function.
        let tx = self.conn.unchecked_transaction()?;
        let now = Utc::now().to_rfc3339();

        let reflections = tx.execute(
            "UPDATE reflections SET repo = ?1, updated_at = ?3 WHERE repo = ?2",
            rusqlite::params![to, from, &now],
        )? as u64;

        let tasks_from = tx.execute(
            "UPDATE tasks SET from_repo = ?1 WHERE from_repo = ?2",
            [to, from],
        )? as u64;

        let tasks_to = tx.execute(
            "UPDATE tasks SET to_repo = ?1 WHERE to_repo = ?2",
            [to, from],
        )? as u64;

        // Delete target rows first to avoid PRIMARY KEY collision,
        // then rename. The old read-state for `to` is stale anyway.
        tx.execute("DELETE FROM board_reads WHERE reader_repo = ?1", [to])?;
        let board_reads = tx.execute(
            "UPDATE board_reads SET reader_repo = ?1 WHERE reader_repo = ?2",
            [to, from],
        )? as u64;

        // Same for watch_handled: delete target's rows first to
        // avoid composite PK collision on (signal_id, repo_name).
        tx.execute("DELETE FROM watch_handled WHERE repo_name = ?1", [to])?;
        let watch_handled = tx.execute(
            "UPDATE watch_handled SET repo_name = ?1 WHERE repo_name = ?2",
            [to, from],
        )? as u64;

        let schedules = tx.execute(
            "UPDATE schedules SET repo = ?1, updated_at = ?3 WHERE repo = ?2",
            rusqlite::params![to, from, &now],
        )? as u64;

        tx.commit()?;

        Ok(RenameCounts {
            reflections,
            tasks_from,
            tasks_to,
            board_reads,
            watch_handled,
            schedules,
        })
    }
}

/// Counts of rows updated by a repo rename.
#[derive(Debug)]
pub struct RenameCounts {
    pub reflections: u64,
    pub tasks_from: u64,
    pub tasks_to: u64,
    pub board_reads: u64,
    pub watch_handled: u64,
    pub schedules: u64,
}

impl RenameCounts {
    pub fn total(&self) -> u64 {
        self.reflections
            + self.tasks_from
            + self.tasks_to
            + self.board_reads
            + self.watch_handled
            + self.schedules
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::db::testutil::test_db;

    #[test]
    fn open_creates_database() {
        let dir = tempfile::tempdir().unwrap();
        let _db = Database::open(&dir.path().join("test.db")).unwrap();
        assert!(dir.path().join("test.db").exists());
    }

    #[test]
    fn open_creates_parent_directories() {
        let dir = tempfile::tempdir().unwrap();
        let nested = dir.path().join("a").join("b").join("c").join("test.db");
        let _db = Database::open(&nested).unwrap();
        assert!(nested.exists());
    }

    #[test]
    fn init_schema_migrates_a_v1_database() {
        // A database created at the original v1 shape (base reflections
        // table only) must come up through every column migration when
        // reopened through the split dispatcher, ending insert-ready.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("old.db");
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(
                "CREATE TABLE reflections (
                    id TEXT PRIMARY KEY,
                    repo TEXT NOT NULL,
                    text TEXT NOT NULL,
                    created_at TEXT NOT NULL,
                    embedding BLOB
                );",
            )
            .unwrap();
            conn.execute(
                "INSERT INTO reflections (id, repo, text, created_at) \
                 VALUES ('old-row', 'legion', 'pre-migration row', '2026-01-01T00:00:00+00:00')",
                [],
            )
            .unwrap();
        }

        let db = Database::open(&path).unwrap();

        // Migration 14 backfill ran: updated_at seeded from created_at.
        let updated: Option<String> = db
            .conn
            .query_row(
                "SELECT updated_at FROM reflections WHERE id = 'old-row'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(updated.as_deref(), Some("2026-01-01T00:00:00+00:00"));

        // The current full-column write and read paths work on the
        // migrated database.
        let r = db
            .insert_reflection("legion", "post-migration row", "team")
            .unwrap();
        assert!(db.get_reflection_by_id(&r.id).unwrap().is_some());
    }

    #[test]
    fn partial_indexes_created_for_soft_delete() {
        let db = test_db();

        // Query sqlite_master for our partial indexes.
        let mut stmt = db
            .conn
            .prepare("SELECT name FROM sqlite_master WHERE type = 'index' AND name LIKE '%_live'")
            .unwrap();
        let indexes: Vec<String> = stmt
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap();

        // Verify all expected partial indexes exist.
        assert!(
            indexes.contains(&"idx_reflections_repo_live".to_string()),
            "idx_reflections_repo_live should exist"
        );
        assert!(
            indexes.contains(&"idx_reflections_audience_live".to_string()),
            "idx_reflections_audience_live should exist"
        );
        assert!(
            indexes.contains(&"idx_tasks_to_live".to_string()),
            "idx_tasks_to_live should exist"
        );
        assert!(
            indexes.contains(&"idx_tasks_from_live".to_string()),
            "idx_tasks_from_live should exist"
        );
        assert!(
            indexes.contains(&"idx_schedules_repo_live".to_string()),
            "idx_schedules_repo_live should exist"
        );
    }
}
