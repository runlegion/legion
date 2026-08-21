//! Legacy inter-agent task storage over the `tasks` table.
//!
//! Before #931, this table also carried kanban cards (the removed CRUD sat
//! side by side with this task CRUD, sharing one table). The card CRUD, its
//! schema migrations, and its query surface are gone; this file now owns
//! only what `src/task.rs` (`legion task create/list/accept/done/block/
//! unblock`) needs. Card-content columns added by the old migrations
//! (labels, source_url, problem, solution, acceptance, document_id, wake_at,
//! pre_defer_status, ...) still exist on databases that had cards -- DB
//! migrations are one-way and columns are never dropped (rule 13) -- but
//! nothing here reads or writes them going forward; they sit inert on any
//! pre-existing rows.

use chrono::Utc;
use rusqlite::Connection;
use uuid::Uuid;

use super::Database;
use crate::error::{LegionError, Result};

/// Base `tasks` table and the indexes over its original columns.
pub(super) fn create_tables(conn: &Connection) -> Result<()> {
    // Migration 3: Tasks table for agent delegation.
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS tasks (
                id TEXT PRIMARY KEY,
                from_repo TEXT NOT NULL,
                to_repo TEXT NOT NULL,
                text TEXT NOT NULL,
                context TEXT,
                priority TEXT NOT NULL DEFAULT 'med',
                status TEXT NOT NULL DEFAULT 'pending',
                note TEXT,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_tasks_to ON tasks(to_repo, status);
            CREATE INDEX IF NOT EXISTS idx_tasks_from ON tasks(from_repo, status);",
    )?;
    Ok(())
}

/// Column migrations for `tasks`, in their original patch order.
///
/// Only the migration `legion task` still needs (`deleted_at`, for its
/// tombstone/soft-delete filtering and multi-node sync) survives here. The
/// card-only migrations (labels, parent_card_id, source_url, source_type,
/// sort_order, assigned_at, started_at, completed_at, problem, solution,
/// acceptance, document_id, wake_at, pre_defer_status) are removed: a fresh
/// database no longer gets those columns at all, and an existing database's
/// copies of them go inert rather than being dropped (rule 13).
pub(super) fn migrate(conn: &Connection) -> Result<()> {
    // Migration 13: Soft delete support for multi-node sync (#245).
    if !Database::has_column(conn, "tasks", "deleted_at")? {
        conn.execute_batch("ALTER TABLE tasks ADD COLUMN deleted_at TEXT;")?;
    }

    // Migration 15: Partial indexes for soft-deleted rows (#256).
    conn.execute_batch(
        "CREATE INDEX IF NOT EXISTS idx_tasks_to_live \
                 ON tasks(to_repo, status) WHERE deleted_at IS NULL;
             CREATE INDEX IF NOT EXISTS idx_tasks_from_live \
                 ON tasks(from_repo) WHERE deleted_at IS NULL;",
    )?;
    Ok(())
}

impl Database {
    /// Get all tasks regardless of repo (for the legacy `/api/tasks` feed).
    pub fn get_all_tasks(&self) -> Result<Vec<crate::task::Task>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, from_repo, to_repo, text, context, priority, status, note, created_at, updated_at \
             FROM tasks WHERE deleted_at IS NULL ORDER BY created_at DESC",
        )?;
        let rows = stmt.query_map([], crate::task::map_task_row)?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(LegionError::Database)
    }

    // --- Task CRUD ---

    /// Insert a new task and return its UUIDv7 ID.
    pub fn insert_task(
        &self,
        from_repo: &str,
        to_repo: &str,
        text: &str,
        context: Option<&str>,
        priority: &str,
    ) -> Result<String> {
        let id = Uuid::now_v7().to_string();
        let now = Utc::now().to_rfc3339();

        self.conn.execute(
            "INSERT INTO tasks (id, from_repo, to_repo, text, context, priority, status, created_at, updated_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'pending', ?7, ?7)",
            rusqlite::params![&id, from_repo, to_repo, text, &context, priority, &now],
        )?;

        Ok(id)
    }

    /// Retrieve a single task by ID.
    pub fn get_task_by_id(&self, id: &str) -> Result<Option<crate::task::Task>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, from_repo, to_repo, text, context, priority, status, note, created_at, updated_at \
             FROM tasks WHERE id = ?1 AND deleted_at IS NULL",
        )?;
        let mut rows = stmt.query_map([id], crate::task::map_task_row)?;
        match rows.next() {
            Some(row) => Ok(Some(row?)),
            None => Ok(None),
        }
    }

    /// List tasks for a repo filtered by direction (inbound or outbound).
    pub fn get_tasks(
        &self,
        repo: &str,
        direction: crate::task::Direction,
    ) -> Result<Vec<crate::task::Task>> {
        let sql = match direction {
            crate::task::Direction::Inbound => {
                "SELECT id, from_repo, to_repo, text, context, priority, status, note, created_at, updated_at \
                 FROM tasks WHERE to_repo = ?1 AND deleted_at IS NULL ORDER BY created_at DESC"
            }
            crate::task::Direction::Outbound => {
                "SELECT id, from_repo, to_repo, text, context, priority, status, note, created_at, updated_at \
                 FROM tasks WHERE from_repo = ?1 AND deleted_at IS NULL ORDER BY created_at DESC"
            }
        };

        let mut stmt = self.conn.prepare(sql)?;
        let rows = stmt.query_map([repo], crate::task::map_task_row)?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(LegionError::Database)
    }

    /// Update a task's status and optional note. Sets updated_at to now.
    ///
    /// Returns an error if no task with the given ID exists.
    pub fn update_task_status(&self, id: &str, status: &str, note: Option<&str>) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        let rows = self.conn.execute(
            "UPDATE tasks SET status = ?1, note = COALESCE(?2, note), updated_at = ?3 WHERE id = ?4 AND deleted_at IS NULL",
            rusqlite::params![status, &note, &now, id],
        )?;
        if rows == 0 {
            return Err(LegionError::TaskNotFound(id.to_string()));
        }
        Ok(())
    }

    /// Count pending tasks assigned to a repo (for bullpen --count path).
    pub fn count_pending_tasks_for_repo(&self, repo: &str) -> Result<u64> {
        let mut stmt = self
            .conn
            .prepare("SELECT COUNT(*) FROM tasks WHERE to_repo = ?1 AND status = 'pending' AND deleted_at IS NULL")?;
        let count: u64 = stmt
            .query_row([repo], |row| row.get(0))
            .map_err(LegionError::Database)?;
        Ok(count)
    }

    /// Get pending tasks assigned to a repo (for surface output). `range`
    /// applies #786's `created_at` predicate directly in the WHERE clause
    /// (`TimeRange::default()` is unbounded, a no-op).
    pub fn get_pending_tasks_for_repo(
        &self,
        repo: &str,
        range: &crate::timerange::TimeRange,
    ) -> Result<Vec<crate::task::Task>> {
        let range_clause = crate::timerange::TimeRange::sql_clause(2);
        let sql = format!(
            "SELECT id, from_repo, to_repo, text, context, priority, status, note, created_at, updated_at \
             FROM tasks WHERE to_repo = ?1 AND status = 'pending' AND deleted_at IS NULL{range_clause} \
             ORDER BY created_at DESC"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(
            rusqlite::params![repo, range.since_bound()?, range.until_bound()?],
            crate::task::map_task_row,
        )?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(LegionError::Database)
    }

    /// Get active (pending, accepted, blocked) tasks assigned to a repo.
    ///
    /// Used by `legion status` to show the YOUR WORK section.
    pub fn get_active_tasks_for_repo(&self, repo: &str) -> Result<Vec<crate::task::Task>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, from_repo, to_repo, text, context, priority, status, note, created_at, updated_at \
             FROM tasks WHERE to_repo = ?1 AND status IN ('pending', 'accepted', 'blocked') AND deleted_at IS NULL \
             ORDER BY CASE priority WHEN 'high' THEN 0 WHEN 'med' THEN 1 WHEN 'low' THEN 2 END, created_at DESC",
        )?;
        let rows = stmt.query_map([repo], crate::task::map_task_row)?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(LegionError::Database)
    }

    /// Get the most recent updated_at timestamp from tasks.
    pub fn get_max_task_updated_at(&self) -> Result<Option<String>> {
        let mut stmt = self
            .conn
            .prepare("SELECT MAX(updated_at) FROM tasks WHERE deleted_at IS NULL")?;
        let result: Option<String> = stmt
            .query_row([], |row| row.get(0))
            .map_err(LegionError::Database)?;
        Ok(result)
    }

    /// Soft-delete a task row by setting its `deleted_at` timestamp.
    ///
    /// Preserves the row for multi-node sync tombstone propagation: it
    /// becomes invisible to normal queries but still syncs to other nodes
    /// via `get_card_deltas_since`/`apply_card_delta` (db/sync.rs -- named
    /// for the table's history, not its current-day contents; #931).
    #[allow(dead_code)] // exercised by db/sync.rs's tombstone tests; no CLI delete-task verb yet
    pub fn soft_delete_task(&self, id: &str) -> Result<bool> {
        let now = chrono::Utc::now().to_rfc3339();
        let rows = self.conn.execute(
            "UPDATE tasks SET deleted_at = ?1 WHERE id = ?2 AND deleted_at IS NULL",
            rusqlite::params![now, id],
        )?;
        Ok(rows > 0)
    }
}
