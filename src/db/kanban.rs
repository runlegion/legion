//! Kanban storage: legacy task CRUD and card CRUD side by side over the
//! single `tasks` table (the collapse is a tracked follow-on). Owns the
//! tasks DDL and its column migrations.

use chrono::Utc;
use rusqlite::Connection;
use uuid::Uuid;

use super::Database;
use crate::error::{LegionError, Result};

/// Which timestamp column to set during a card status update.
#[derive(Debug, Clone, Copy)]
#[allow(dead_code)]
pub enum CardTimestamp {
    Assigned,
    Started,
    Completed,
    None,
}

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
pub(super) fn migrate(conn: &Connection) -> Result<()> {
    // Migration 9: Kanban upgrade -- new columns on tasks table.
    if !Database::has_column(conn, "tasks", "labels")? {
        conn.execute_batch("ALTER TABLE tasks ADD COLUMN labels TEXT")?;
    }
    if !Database::has_column(conn, "tasks", "parent_card_id")? {
        conn.execute_batch("ALTER TABLE tasks ADD COLUMN parent_card_id TEXT")?;
    }
    if !Database::has_column(conn, "tasks", "source_url")? {
        conn.execute_batch("ALTER TABLE tasks ADD COLUMN source_url TEXT")?;
    }
    if !Database::has_column(conn, "tasks", "source_type")? {
        conn.execute_batch("ALTER TABLE tasks ADD COLUMN source_type TEXT")?;
    }
    if !Database::has_column(conn, "tasks", "sort_order")? {
        conn.execute_batch("ALTER TABLE tasks ADD COLUMN sort_order INTEGER NOT NULL DEFAULT 0")?;
    }
    if !Database::has_column(conn, "tasks", "assigned_at")? {
        conn.execute_batch("ALTER TABLE tasks ADD COLUMN assigned_at TEXT")?;
    }
    if !Database::has_column(conn, "tasks", "started_at")? {
        conn.execute_batch("ALTER TABLE tasks ADD COLUMN started_at TEXT")?;
    }
    if !Database::has_column(conn, "tasks", "completed_at")? {
        conn.execute_batch("ALTER TABLE tasks ADD COLUMN completed_at TEXT")?;
    }

    // Structured card fields parsed from issue body context.
    if !Database::has_column(conn, "tasks", "problem")? {
        conn.execute_batch("ALTER TABLE tasks ADD COLUMN problem TEXT")?;
        conn.execute_batch("ALTER TABLE tasks ADD COLUMN solution TEXT")?;
        conn.execute_batch("ALTER TABLE tasks ADD COLUMN acceptance TEXT")?;
        Database::backfill_parsed_fields(conn)?;
    }

    conn.execute_batch(
            "CREATE INDEX IF NOT EXISTS idx_tasks_parent ON tasks(parent_card_id);
             CREATE INDEX IF NOT EXISTS idx_tasks_status_sort ON tasks(status, sort_order, created_at);",
        )?;
    // Backfill timestamps for existing tasks based on current status.
    conn.execute_batch(
        "UPDATE tasks SET completed_at = updated_at \
             WHERE status IN ('done', 'cancelled') AND completed_at IS NULL;
             UPDATE tasks SET started_at = updated_at \
             WHERE status = 'accepted' AND started_at IS NULL;
             UPDATE tasks SET assigned_at = created_at \
             WHERE status != 'backlog' AND assigned_at IS NULL;",
    )?;

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
    /// Parse context into structured fields for cards that have context but no parsed data.
    fn backfill_parsed_fields(conn: &Connection) -> Result<()> {
        let mut stmt = conn.prepare(
            "SELECT id, context FROM tasks WHERE context IS NOT NULL AND problem IS NULL",
        )?;
        let rows: Vec<(String, String)> = stmt
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(LegionError::Database)?;

        if rows.is_empty() {
            return Ok(());
        }

        for (id, context) in &rows {
            let parsed = crate::card_parse::parse_issue_body(context);
            let acceptance = if parsed.acceptance.is_empty() {
                None
            } else {
                Some(parsed.acceptance.join("\n"))
            };
            conn.execute(
                "UPDATE tasks SET problem = ?1, solution = ?2, acceptance = ?3 WHERE id = ?4",
                rusqlite::params![parsed.problem, parsed.solution, acceptance, id],
            )?;
        }
        Ok(())
    }

    /// Get all tasks regardless of repo (for kanban view).
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

    /// Get pending tasks assigned to a repo (for surface output).
    pub fn get_pending_tasks_for_repo(&self, repo: &str) -> Result<Vec<crate::task::Task>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, from_repo, to_repo, text, context, priority, status, note, created_at, updated_at \
             FROM tasks WHERE to_repo = ?1 AND status = 'pending' AND deleted_at IS NULL ORDER BY created_at DESC",
        )?;
        let rows = stmt.query_map([repo], crate::task::map_task_row)?;
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

    // --- Card CRUD (kanban) ---

    /// The full column list for card queries.
    const CARD_COLUMNS: &'static str = "id, from_repo, to_repo, text, context, priority, status, note, \
         labels, parent_card_id, source_url, source_type, sort_order, \
         created_at, updated_at, assigned_at, started_at, completed_at, \
         problem, solution, acceptance";

    /// SQL fragment for consistent priority ordering across all card queries.
    const PRIORITY_ORDER: &'static str = "CASE priority WHEN 'critical' THEN 0 WHEN 'high' THEN 1 \
         WHEN 'med' THEN 2 WHEN 'low' THEN 3 END";

    /// Insert a new kanban card and return its UUIDv7 ID.
    #[allow(clippy::too_many_arguments)]
    pub fn insert_card(
        &self,
        from_repo: &str,
        to_repo: &str,
        text: &str,
        context: Option<&str>,
        priority: &str,
        labels: Option<&str>,
        parent_card_id: Option<&str>,
        source_url: Option<&str>,
        source_type: Option<&str>,
        created_at_override: Option<&str>,
        status: crate::kanban::CardStatus,
    ) -> Result<String> {
        let id = uuid::Uuid::now_v7().to_string();
        let now = chrono::Utc::now().to_rfc3339();
        let created_at = created_at_override.unwrap_or(&now);
        let status_str = status.to_string();

        let parsed = context.map(crate::card_parse::parse_issue_body);
        let problem = parsed.as_ref().and_then(|p| p.problem.as_deref());
        let solution = parsed.as_ref().and_then(|p| p.solution.as_deref());
        let acceptance = parsed
            .as_ref()
            .map(|p| &p.acceptance)
            .filter(|a| !a.is_empty())
            .map(|a| a.join("\n"));

        // NOTE: placeholder numbers are NOT sequential. `status` was added late and
        // binds `?16` (the last param) in the 7th column slot to keep the original
        // ?1..?15 mapping untouched. When adding a new column, append its param to the
        // list and give it the next free number (?17, ...) in the correct column slot
        // -- do not reuse ?16 or assume position == placeholder number.
        self.conn.execute(
            "INSERT INTO tasks (id, from_repo, to_repo, text, context, priority, status, \
             labels, parent_card_id, source_url, source_type, created_at, updated_at, \
             problem, solution, acceptance) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?16, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
            rusqlite::params![
                id,
                from_repo,
                to_repo,
                text,
                context,
                priority,
                labels,
                parent_card_id,
                source_url,
                source_type,
                created_at,
                now,
                problem,
                solution,
                acceptance,
                status_str,
            ],
        )?;
        Ok(id)
    }

    /// Retrieve a single card by ID.
    pub fn get_card_by_id(&self, id: &str) -> Result<Option<crate::kanban::Card>> {
        let sql = format!(
            "SELECT {} FROM tasks WHERE id = ?1 AND deleted_at IS NULL",
            Self::CARD_COLUMNS
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let mut rows = stmt.query_map([id], crate::kanban::map_card_row)?;
        match rows.next() {
            Some(row) => Ok(Some(row.map_err(LegionError::Database)?)),
            None => Ok(None),
        }
    }

    /// List cards for a repo filtered by direction.
    pub fn get_cards(
        &self,
        repo: &str,
        direction: crate::kanban::Direction,
        scope: crate::kanban::CardScope,
    ) -> Result<Vec<crate::kanban::Card>> {
        // Status predicate for the requested slice of the board. WorkingSet is the
        // default consumer view (active work); Backlog is the raw inbox; All keeps
        // every non-deleted row. Status literals match CardStatus::Display.
        let status_filter = match scope {
            crate::kanban::CardScope::WorkingSet => {
                " AND status NOT IN ('backlog', 'done', 'cancelled')"
            }
            crate::kanban::CardScope::Backlog => " AND status = 'backlog'",
            crate::kanban::CardScope::All => "",
        };
        let sql = match direction {
            crate::kanban::Direction::Inbound => {
                format!(
                    "SELECT {} FROM tasks WHERE to_repo = ?1 AND deleted_at IS NULL{} ORDER BY {}, sort_order ASC, created_at DESC",
                    Self::CARD_COLUMNS,
                    status_filter,
                    Self::PRIORITY_ORDER
                )
            }
            crate::kanban::Direction::Outbound => {
                format!(
                    "SELECT {} FROM tasks WHERE from_repo = ?1 AND deleted_at IS NULL{} ORDER BY created_at DESC",
                    Self::CARD_COLUMNS,
                    status_filter
                )
            }
        };
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map([repo], crate::kanban::map_card_row)?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(LegionError::Database)
    }

    /// Get all cards for the kanban board view.
    pub fn get_all_cards(&self) -> Result<Vec<crate::kanban::Card>> {
        let sql = format!(
            "SELECT {} FROM tasks WHERE deleted_at IS NULL ORDER BY {}, sort_order ASC, created_at DESC",
            Self::CARD_COLUMNS,
            Self::PRIORITY_ORDER
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map([], crate::kanban::map_card_row)?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(LegionError::Database)
    }

    /// Count pending cards assigned to a repo.
    pub fn count_pending_cards_for_repo(&self, repo: &str) -> Result<u64> {
        let mut stmt = self
            .conn
            .prepare("SELECT COUNT(*) FROM tasks WHERE to_repo = ?1 AND status = 'pending' AND deleted_at IS NULL")?;
        let count: u64 = stmt
            .query_row([repo], |row| row.get(0))
            .map_err(LegionError::Database)?;
        Ok(count)
    }

    /// Get pending cards assigned to a repo.
    pub fn get_pending_cards_for_repo(&self, repo: &str) -> Result<Vec<crate::kanban::Card>> {
        let sql = format!(
            "SELECT {} FROM tasks WHERE to_repo = ?1 AND status = 'pending' AND deleted_at IS NULL \
             ORDER BY {}, sort_order ASC, created_at ASC",
            Self::CARD_COLUMNS,
            Self::PRIORITY_ORDER
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map([repo], crate::kanban::map_card_row)?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(LegionError::Database)
    }

    /// Get active cards for a repo (all non-done/non-cancelled).
    #[allow(dead_code)]
    pub fn get_active_cards_for_repo(&self, repo: &str) -> Result<Vec<crate::kanban::Card>> {
        let sql = format!(
            "SELECT {} FROM tasks WHERE to_repo = ?1 AND status NOT IN ('done', 'cancelled') AND deleted_at IS NULL \
             ORDER BY {}, sort_order ASC, created_at DESC",
            Self::CARD_COLUMNS,
            Self::PRIORITY_ORDER
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map([repo], crate::kanban::map_card_row)?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(LegionError::Database)
    }

    /// Atomically pick the next pending card for a repo and accept it.
    ///
    /// Selects highest priority, then lowest sort_order, then oldest.
    /// Transitions to accepted and sets started_at. Returns None if empty.
    pub fn pick_next_card(&self, repo: &str) -> Result<Option<crate::kanban::Card>> {
        let sql = format!(
            "SELECT {} FROM tasks WHERE to_repo = ?1 AND status = 'pending' AND deleted_at IS NULL \
             ORDER BY {}, sort_order ASC, created_at ASC LIMIT 1",
            Self::CARD_COLUMNS,
            Self::PRIORITY_ORDER
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let mut rows = stmt.query_map([repo], crate::kanban::map_card_row)?;
        let card = match rows.next() {
            Some(row) => row.map_err(LegionError::Database)?,
            None => return Ok(None),
        };
        drop(rows);
        drop(stmt);

        let now = chrono::Utc::now().to_rfc3339();
        let rows_affected = self.conn.execute(
            "UPDATE tasks SET status = 'accepted', started_at = ?1, updated_at = ?2 \
             WHERE id = ?3 AND status = 'pending' AND deleted_at IS NULL",
            rusqlite::params![now, now, card.id],
        )?;
        if rows_affected == 0 {
            return Ok(None);
        }

        self.get_card_by_id(&card.id)
    }

    /// Peek at the next pending card without accepting it.
    pub fn peek_next_card(&self, repo: &str) -> Result<Option<crate::kanban::Card>> {
        let sql = format!(
            "SELECT {} FROM tasks WHERE to_repo = ?1 AND status = 'pending' AND deleted_at IS NULL \
             ORDER BY {}, sort_order ASC, created_at ASC LIMIT 1",
            Self::CARD_COLUMNS,
            Self::PRIORITY_ORDER
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let mut rows = stmt.query_map([repo], crate::kanban::map_card_row)?;
        match rows.next() {
            Some(row) => Ok(Some(row.map_err(LegionError::Database)?)),
            None => Ok(None),
        }
    }

    /// Update a card's status with timestamp tracking.
    pub fn update_card_status(
        &self,
        id: &str,
        status: &str,
        note: Option<&str>,
        timestamp: CardTimestamp,
    ) -> Result<()> {
        let now = chrono::Utc::now().to_rfc3339();

        let rows = match timestamp {
            CardTimestamp::Assigned => self.conn.execute(
                "UPDATE tasks SET status = ?1, note = COALESCE(?2, note), \
                 assigned_at = ?3, updated_at = ?4 WHERE id = ?5 AND deleted_at IS NULL",
                rusqlite::params![status, note, now, now, id],
            )?,
            CardTimestamp::Started => self.conn.execute(
                "UPDATE tasks SET status = ?1, note = COALESCE(?2, note), \
                 started_at = ?3, updated_at = ?4 WHERE id = ?5 AND deleted_at IS NULL",
                rusqlite::params![status, note, now, now, id],
            )?,
            CardTimestamp::Completed => self.conn.execute(
                "UPDATE tasks SET status = ?1, note = COALESCE(?2, note), \
                 completed_at = ?3, updated_at = ?4 WHERE id = ?5 AND deleted_at IS NULL",
                rusqlite::params![status, note, now, now, id],
            )?,
            CardTimestamp::None => self.conn.execute(
                "UPDATE tasks SET status = ?1, note = COALESCE(?2, note), \
                 updated_at = ?3 WHERE id = ?4 AND deleted_at IS NULL",
                rusqlite::params![status, note, now, id],
            )?,
        };
        if rows == 0 {
            return Err(LegionError::CardNotFound(id.to_string()));
        }
        Ok(())
    }

    /// Force-move a card to any status (bypasses state machine).
    pub fn force_move_card(&self, id: &str, status: &str, sort_order: Option<i32>) -> Result<()> {
        let now = chrono::Utc::now().to_rfc3339();
        let sort = sort_order.unwrap_or(0);
        // Set the appropriate timestamp based on target status
        let ts_sql = match status {
            "done" | "cancelled" => ", completed_at = ?5",
            "accepted" | "in-review" | "needs-input" => ", started_at = COALESCE(started_at, ?5)",
            "pending" => ", assigned_at = COALESCE(assigned_at, ?5)",
            _ => "",
        };
        let sql = format!(
            "UPDATE tasks SET status = ?1, sort_order = ?2, updated_at = ?3{ts_sql} WHERE id = ?4 AND deleted_at IS NULL"
        );
        let rows = if ts_sql.is_empty() {
            self.conn
                .execute(&sql, rusqlite::params![status, sort, now, id])?
        } else {
            self.conn
                .execute(&sql, rusqlite::params![status, sort, now, id, now])?
        };
        if rows == 0 {
            return Err(LegionError::CardNotFound(id.to_string()));
        }
        Ok(())
    }

    /// Assign a backlog card to an agent and transition to pending.
    ///
    /// Atomic: updates to_repo, status, and assigned_at in one statement.
    /// Only works on backlog cards -- returns InvalidCardTransition otherwise.
    pub fn assign_card(&self, id: &str, to_repo: &str) -> Result<()> {
        let now = chrono::Utc::now().to_rfc3339();
        let rows = self.conn.execute(
            "UPDATE tasks SET to_repo = ?1, status = 'pending', \
             assigned_at = ?2, updated_at = ?3 WHERE id = ?4 AND status = 'backlog' AND deleted_at IS NULL",
            rusqlite::params![to_repo, now, now, id],
        )?;
        if rows == 0 {
            let exists = self.get_card_by_id(id)?;
            return match exists {
                None => Err(LegionError::CardNotFound(id.to_string())),
                Some(card) => Err(LegionError::InvalidCardTransition {
                    action: "assign".to_string(),
                    current: card.status.to_string(),
                }),
            };
        }
        Ok(())
    }

    /// Permanently remove a kanban card from the database.
    ///
    /// Unlike `transition_card` with `Cancel`, which moves the card to a
    /// terminal `cancelled` state where it still appears in `legion kanban
    /// list`, this drops the row entirely. Used to hard-remove a card
    /// filed in error (e.g. a card created from a mistaken
    /// `legion kanban create`) that should never have existed. Returns
    /// `CardNotFound` if the id does not match any row so the caller can
    /// surface a clear error rather than silently no-op.
    pub fn delete_card(&self, id: &str) -> Result<()> {
        let rows = self
            .conn
            .execute("DELETE FROM tasks WHERE id = ?1", rusqlite::params![id])?;
        if rows == 0 {
            return Err(LegionError::CardNotFound(id.to_string()));
        }
        Ok(())
    }

    /// Soft-delete a card by setting its deleted_at timestamp.
    ///
    /// Unlike `delete_card` (hard delete), this preserves the row for
    /// multi-node sync tombstone propagation. The row becomes invisible to
    /// normal queries but can still be synced to other nodes.
    #[allow(dead_code)] // Used by sync module in #248
    pub fn soft_delete_card(&self, id: &str) -> Result<bool> {
        let now = chrono::Utc::now().to_rfc3339();
        let rows = self.conn.execute(
            "UPDATE tasks SET deleted_at = ?1 WHERE id = ?2 AND deleted_at IS NULL",
            rusqlite::params![now, id],
        )?;
        Ok(rows > 0)
    }

    /// Get per-agent workload summary.
    pub fn get_agent_workloads(&self) -> Result<Vec<crate::kanban::AgentWorkload>> {
        let mut stmt = self.conn.prepare(
            "SELECT to_repo, \
             SUM(CASE WHEN status IN ('accepted', 'in-review', 'needs-input') THEN 1 ELSE 0 END) as active, \
             SUM(CASE WHEN status = 'pending' THEN 1 ELSE 0 END) as pending, \
             SUM(CASE WHEN status = 'blocked' THEN 1 ELSE 0 END) as blocked \
             FROM tasks WHERE status NOT IN ('done', 'cancelled') AND deleted_at IS NULL \
             GROUP BY to_repo ORDER BY to_repo",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(crate::kanban::AgentWorkload {
                repo: row.get(0)?,
                active: row.get(1)?,
                pending: row.get(2)?,
                blocked: row.get(3)?,
            })
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(LegionError::Database)
    }

    /// Update mutable card fields by ID.
    ///
    /// Builds a SET clause only for fields that are Some, so callers can
    /// update one field at a time without touching the others. Always
    /// sets `updated_at` to now. Returns `CardNotFound` if the id does not
    /// exist.
    #[allow(clippy::too_many_arguments)]
    pub fn update_card_fields(
        &self,
        id: &str,
        text: Option<&str>,
        context: Option<&str>,
        problem: Option<&str>,
        solution: Option<&str>,
        acceptance: Option<&str>,
        priority: Option<&str>,
        labels: Option<&str>,
    ) -> Result<()> {
        let now = chrono::Utc::now().to_rfc3339();
        let mut sets: Vec<String> = Vec::new();
        let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();

        if let Some(v) = text {
            sets.push(format!("text = ?{}", params.len() + 1));
            params.push(Box::new(v.to_string()));
        }
        if let Some(v) = context {
            sets.push(format!("context = ?{}", params.len() + 1));
            params.push(Box::new(v.to_string()));
        }
        if let Some(v) = problem {
            sets.push(format!("problem = ?{}", params.len() + 1));
            params.push(Box::new(v.to_string()));
        }
        if let Some(v) = solution {
            sets.push(format!("solution = ?{}", params.len() + 1));
            params.push(Box::new(v.to_string()));
        }
        if let Some(v) = acceptance {
            sets.push(format!("acceptance = ?{}", params.len() + 1));
            params.push(Box::new(v.to_string()));
        }
        if let Some(v) = priority {
            sets.push(format!("priority = ?{}", params.len() + 1));
            params.push(Box::new(v.to_string()));
        }
        if let Some(v) = labels {
            sets.push(format!("labels = ?{}", params.len() + 1));
            params.push(Box::new(v.to_string()));
        }

        // updated_at is always set
        sets.push(format!("updated_at = ?{}", params.len() + 1));
        params.push(Box::new(now));

        let id_pos = params.len() + 1;
        params.push(Box::new(id.to_string()));

        let sql = format!(
            "UPDATE tasks SET {} WHERE id = ?{} AND deleted_at IS NULL",
            sets.join(", "),
            id_pos
        );
        let param_refs: Vec<&dyn rusqlite::types::ToSql> =
            params.iter().map(|p| p.as_ref()).collect();
        let rows = self.conn.execute(&sql, param_refs.as_slice())?;
        if rows == 0 {
            return Err(LegionError::CardNotFound(id.to_string()));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::db::testutil::test_db;

    #[test]
    fn delete_card_removes_row_and_reports_not_found() {
        let db = test_db();

        // Insert a minimal card and confirm it is visible before delete.
        let id = db
            .insert_card(
                "legion",
                "legion",
                "test card to delete",
                None,
                "med",
                None,
                None,
                None,
                None,
                None,
                crate::kanban::CardStatus::Backlog,
            )
            .unwrap();
        assert!(db.get_card_by_id(&id).unwrap().is_some());

        // Delete the card and confirm it is gone.
        db.delete_card(&id).expect("delete existing card");
        assert!(db.get_card_by_id(&id).unwrap().is_none());

        // Deleting a non-existent card returns CardNotFound, not a silent
        // no-op.
        let result = db.delete_card(&id);
        assert!(
            matches!(result, Err(LegionError::CardNotFound(_))),
            "expected CardNotFound for missing card, got: {result:?}"
        );
    }

    #[test]
    fn soft_delete_card_hides_from_queries() {
        let db = test_db();

        // Insert a card.
        let id = db
            .insert_card(
                "legion",
                "legion",
                "soft delete test card",
                None,
                "med",
                None,
                None,
                None,
                None,
                None,
                crate::kanban::CardStatus::Backlog,
            )
            .unwrap();
        assert!(db.get_card_by_id(&id).unwrap().is_some());

        // Soft delete it.
        let deleted = db.soft_delete_card(&id).unwrap();
        assert!(deleted, "soft_delete_card should return true");

        // The card should now be invisible to normal queries.
        assert!(
            db.get_card_by_id(&id).unwrap().is_none(),
            "soft-deleted card should not be visible"
        );

        // Soft deleting again returns false (already deleted).
        let deleted_again = db.soft_delete_card(&id).unwrap();
        assert!(
            !deleted_again,
            "soft_delete_card on already-deleted should return false"
        );
    }
}
