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

    // Migration 16: card<->document binding (#528).
    // TEXT NULL; application-layer uniqueness enforced in bind_card_to_document.
    // No REFERENCES clause -- PRAGMA foreign_keys is not enabled globally.
    if !Database::has_column(conn, "tasks", "document_id")? {
        conn.execute_batch("ALTER TABLE tasks ADD COLUMN document_id TEXT;")?;
        conn.execute_batch(
            "CREATE INDEX IF NOT EXISTS idx_tasks_document_id \
             ON tasks(document_id) WHERE document_id IS NOT NULL;",
        )?;
    }

    // Migration 17: kanban defer -- card-level scheduled wake (#816).
    // `wake_at` (RFC3339, matching `created_at`'s format) is the time the
    // deferred card wakes; `pre_defer_status` is the status the card was in
    // immediately before deferring, so the revert target (Accepted or
    // Pending -- Defer is legal from either) is data-dependent, not fixed
    // by the state machine alone. Both TEXT NULL; only set while a card is
    // Deferred, cleared on revert.
    if !Database::has_column(conn, "tasks", "wake_at")? {
        conn.execute_batch("ALTER TABLE tasks ADD COLUMN wake_at TEXT;")?;
    }
    if !Database::has_column(conn, "tasks", "pre_defer_status")? {
        conn.execute_batch("ALTER TABLE tasks ADD COLUMN pre_defer_status TEXT;")?;
    }
    // Partial index: `tick_health`'s sweep polls "deferred cards due now"
    // every health tick (src/watch/mod.rs), so this predicate runs
    // constantly on every board regardless of how many cards are deferred.
    conn.execute_batch(
        "CREATE INDEX IF NOT EXISTS idx_tasks_deferred_wake_at \
         ON tasks(wake_at) WHERE status = 'deferred';",
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

    // --- Card CRUD (kanban) ---

    /// The full column list for card queries.
    ///
    /// Column index reference (0-based, matches `map_card_row`):
    ///  0  id              8  labels          16 started_at
    ///  1  from_repo       9  parent_card_id  17 completed_at
    ///  2  to_repo        10  source_url       18 problem
    ///  3  text           11  source_type      19 solution
    ///  4  context        12  sort_order       20 acceptance
    ///  5  priority       13  created_at       21 document_id
    ///  6  status         14  updated_at       22 wake_at
    ///  7  note           15  assigned_at      23 pre_defer_status
    const CARD_COLUMNS: &'static str = "id, from_repo, to_repo, text, context, priority, status, note, \
         labels, parent_card_id, source_url, source_type, sort_order, \
         created_at, updated_at, assigned_at, started_at, completed_at, \
         problem, solution, acceptance, document_id, wake_at, pre_defer_status";

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
        priority: crate::kanban::Priority,
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
        let priority_str = priority.to_string();

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
                priority_str,
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
        // default consumer view (active work); Backlog is the raw inbox; Deferred
        // is the consciously-separate "put off until later" bucket (#816 -- a
        // deferred card must be visible somewhere, never silently uncounted, so
        // it gets its own scope rather than folding into WorkingSet or vanishing
        // between Backlog and All); All keeps every non-deleted row. Status
        // literals match CardStatus::Display.
        let status_filter = match scope {
            crate::kanban::CardScope::WorkingSet => {
                " AND status NOT IN ('backlog', 'done', 'cancelled', 'deferred')"
            }
            crate::kanban::CardScope::Backlog => " AND status = 'backlog'",
            crate::kanban::CardScope::Deferred => " AND status = 'deferred'",
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

    /// Get every card currently in `Delegated` (#778): the delegated-work
    /// liveness sweep target. `repo` filters to one board (`to_repo`,
    /// matching how `get_cards` scopes inbound work) when `Some`; `None`
    /// scans every repo, which is what `tick_health`'s auto-revert sweep
    /// needs since a single watch daemon can service many repos. One query
    /// handles both: `?1 IS NULL` short-circuits the filter when `repo` is
    /// `None` (rusqlite binds `Option<&str>` as SQL NULL).
    pub fn get_delegated_cards(&self, repo: Option<&str>) -> Result<Vec<crate::kanban::Card>> {
        let sql = format!(
            "SELECT {} FROM tasks \
             WHERE (?1 IS NULL OR to_repo = ?1) AND status = 'delegated' AND deleted_at IS NULL \
             ORDER BY {}, sort_order ASC, created_at ASC",
            Self::CARD_COLUMNS,
            Self::PRIORITY_ORDER
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params![repo], crate::kanban::map_card_row)?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(LegionError::Database)
    }

    /// Get every `Deferred` card whose `wake_at` has passed (#816): the
    /// scheduled-wake sweep target for `tick_health`. Scans every repo (no
    /// `repo` filter), the same "one watch daemon can service many boards"
    /// reasoning `get_delegated_cards` documents for its own sweep use.
    pub fn get_deferred_cards_due(&self, now: &str) -> Result<Vec<crate::kanban::Card>> {
        let sql = format!(
            "SELECT {} FROM tasks \
             WHERE status = 'deferred' AND wake_at IS NOT NULL AND wake_at <= ?1 \
             AND deleted_at IS NULL \
             ORDER BY {}, sort_order ASC, created_at ASC",
            Self::CARD_COLUMNS,
            Self::PRIORITY_ORDER
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map([now], crate::kanban::map_card_row)?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(LegionError::Database)
    }

    /// Atomically transition a card to `Deferred` and stamp `wake_at`/
    /// `pre_defer_status` (#816 review fix, HIGH): the previous shape called
    /// `transition_card_status_with_sync` and a separate field-stamp as two
    /// writes. A crash (or a `set_card_defer_fields`-style error) between
    /// them left a `Deferred` card with `wake_at = NULL` -- and
    /// `get_deferred_cards_due`'s `wake_at IS NOT NULL` filter permanently
    /// excludes such a row from the sweep, unlike `Delegated`'s equivalent
    /// partial-failure window (`get_delegated_cards` has no link-presence
    /// filter, so it stays reapable either way). A single `UPDATE` statement
    /// is atomic by construction (SQLite commits or rolls back one
    /// statement as a unit even outside an explicit transaction), so this
    /// closes the gap without needing `unchecked_transaction`.
    ///
    /// `kanban::defer_card` is the only caller; it does not sync a bound
    /// document the way `transition_card_status_with_sync` does for other
    /// transitions, because `requirement_status_for_card_status` maps
    /// `Deferred` to `None` (deferring carries no spec meaning).
    pub fn set_card_deferred(
        &self,
        id: &str,
        note: Option<&str>,
        wake_at: &str,
        pre_defer_status: &str,
    ) -> Result<()> {
        let now = chrono::Utc::now().to_rfc3339();
        let rows = self.conn.execute(
            "UPDATE tasks SET status = 'deferred', note = COALESCE(?1, note), \
             wake_at = ?2, pre_defer_status = ?3, updated_at = ?4 \
             WHERE id = ?5 AND deleted_at IS NULL",
            rusqlite::params![note, wake_at, pre_defer_status, now, id],
        )?;
        if rows == 0 {
            return Err(LegionError::CardNotFound(id.to_string()));
        }
        Ok(())
    }

    /// Clear `wake_at`/`pre_defer_status` on a card leaving `Deferred`
    /// (#816). Called by `kanban::undefer_card` (both the manual CLI path
    /// and `tick_health`'s auto-wake sweep) so a resolved defer never
    /// leaves a stale wake_at behind -- mirroring
    /// `clear_wake_attempt_card`'s role for `undelegate_card`.
    pub fn clear_card_defer_fields(&self, id: &str) -> Result<()> {
        let rows = self.conn.execute(
            "UPDATE tasks SET wake_at = NULL, pre_defer_status = NULL WHERE id = ?1 AND deleted_at IS NULL",
            [id],
        )?;
        if rows == 0 {
            return Err(LegionError::CardNotFound(id.to_string()));
        }
        Ok(())
    }

    /// Atomically pick the next pending card for a repo and accept it.
    ///
    /// Selects highest priority, then lowest sort_order, then oldest.
    /// Transitions to Accepted and sets started_at. Returns None if empty
    /// or if the card was raced away (another picker accepted it first).
    ///
    /// The `expected_from_status = Some("pending")` argument to
    /// `transition_card_status_with_sync` ensures only one concurrent picker
    /// wins: the UPDATE's `AND status = 'pending'` predicate makes it
    /// conditional, so the second caller gets `rows_affected == 0` -> `CardNotFound`
    /// -> `Ok(None)` here, matching the original direct-UPDATE semantics.
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

        // Pass expected_from_status = Some("pending") so the UPDATE is
        // conditional on the card still being pending at write time.
        // CardNotFound covers both "card deleted" and "status already changed"
        // (lost race) -- both map to Ok(None).
        match self.transition_card_status_with_sync(
            &card.id,
            &crate::kanban::CardStatus::Accepted.to_string(),
            None,
            CardTimestamp::Started,
            card.document_id.as_deref(),
            Some("pending"),
        ) {
            Ok(()) => {}
            Err(LegionError::CardNotFound(_)) => return Ok(None),
            Err(e) => return Err(e),
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

    /// Map a typed `CardStatus` to its requirement `meta.status` equivalent
    /// for the v2 requirement schema status enum (#528).
    ///
    /// Returns `None` for statuses that carry no spec meaning (the spec stays
    /// where it is). Returns `Some(status_str)` for the four mapped states.
    /// The match is exhaustive with no catch-all so a new `CardStatus` variant
    /// forces a compile-time decision here.
    pub(crate) fn requirement_status_for_card_status(
        card_status: crate::kanban::CardStatus,
    ) -> Option<&'static str> {
        use crate::kanban::CardStatus;
        match card_status {
            // Mapped: these transitions carry spec meaning.
            CardStatus::Accepted => Some("accepted"),
            CardStatus::InReview => Some("implemented"),
            CardStatus::Done => Some("verified"),
            CardStatus::Cancelled => Some("cancelled"),
            // Unmapped: spec stays where it is.
            CardStatus::Backlog => None,
            CardStatus::Pending => None,
            CardStatus::NeedsInput => None,
            CardStatus::Blocked => None,
            CardStatus::Delegated => None,
            // Deferred carries no spec meaning either -- the bound document
            // (if any) stays exactly where the pre-defer status left it;
            // deferring and later waking do not move the spec's own status.
            CardStatus::Deferred => None,
        }
    }

    /// Transition a card's status and -- if the card has a bound document --
    /// update that document's `meta.status` and hoisted `status` column in
    /// a single atomic transaction.
    ///
    /// When `document_id` is `Some`, the payload parse must succeed; a bound
    /// document with an unparseable payload fails the whole transition so
    /// neither the card nor the spec drift silently.
    ///
    /// When `document_id` is `None` this behaves identically to the old
    /// `update_card_status` path but inside an `unchecked_transaction`.
    ///
    /// `expected_from_status`: when `Some(s)`, the UPDATE predicate includes
    /// `AND status = s` so the write is conditional on the card still being in
    /// that exact status at write time. A mismatch (rows_affected == 0) maps
    /// to `CardNotFound`, letting callers treat a lost race as "no card to
    /// update" without a separate read-modify-write. `pick_next_card` passes
    /// `Some("pending")` to restore the atomicity guarantee the prior direct
    /// UPDATE had. All other callers pass `None`.
    pub fn transition_card_status_with_sync(
        &self,
        id: &str,
        status: &str,
        note: Option<&str>,
        timestamp: CardTimestamp,
        document_id: Option<&str>,
        expected_from_status: Option<&str>,
    ) -> Result<()> {
        let now = chrono::Utc::now().to_rfc3339();

        let tx = self.conn.unchecked_transaction()?;

        // -- update the card row --
        // When expected_from_status is Some, the WHERE clause includes
        // `AND status = ?` so the update is conditional on the card still
        // being in the expected status at commit time.
        let rows_affected = match (timestamp, expected_from_status) {
            (CardTimestamp::Assigned, None) => tx.execute(
                "UPDATE tasks SET status = ?1, note = COALESCE(?2, note), \
                 assigned_at = ?3, updated_at = ?4 WHERE id = ?5 AND deleted_at IS NULL",
                rusqlite::params![status, note, now, now, id],
            )?,
            (CardTimestamp::Assigned, Some(from)) => tx.execute(
                "UPDATE tasks SET status = ?1, note = COALESCE(?2, note), \
                 assigned_at = ?3, updated_at = ?4 \
                 WHERE id = ?5 AND status = ?6 AND deleted_at IS NULL",
                rusqlite::params![status, note, now, now, id, from],
            )?,
            (CardTimestamp::Started, None) => tx.execute(
                "UPDATE tasks SET status = ?1, note = COALESCE(?2, note), \
                 started_at = ?3, updated_at = ?4 WHERE id = ?5 AND deleted_at IS NULL",
                rusqlite::params![status, note, now, now, id],
            )?,
            (CardTimestamp::Started, Some(from)) => tx.execute(
                "UPDATE tasks SET status = ?1, note = COALESCE(?2, note), \
                 started_at = ?3, updated_at = ?4 \
                 WHERE id = ?5 AND status = ?6 AND deleted_at IS NULL",
                rusqlite::params![status, note, now, now, id, from],
            )?,
            (CardTimestamp::Completed, None) => tx.execute(
                "UPDATE tasks SET status = ?1, note = COALESCE(?2, note), \
                 completed_at = ?3, updated_at = ?4 WHERE id = ?5 AND deleted_at IS NULL",
                rusqlite::params![status, note, now, now, id],
            )?,
            (CardTimestamp::Completed, Some(from)) => tx.execute(
                "UPDATE tasks SET status = ?1, note = COALESCE(?2, note), \
                 completed_at = ?3, updated_at = ?4 \
                 WHERE id = ?5 AND status = ?6 AND deleted_at IS NULL",
                rusqlite::params![status, note, now, now, id, from],
            )?,
            (CardTimestamp::None, None) => tx.execute(
                "UPDATE tasks SET status = ?1, note = COALESCE(?2, note), \
                 updated_at = ?3 WHERE id = ?4 AND deleted_at IS NULL",
                rusqlite::params![status, note, now, id],
            )?,
            (CardTimestamp::None, Some(from)) => tx.execute(
                "UPDATE tasks SET status = ?1, note = COALESCE(?2, note), \
                 updated_at = ?3 WHERE id = ?4 AND status = ?5 AND deleted_at IS NULL",
                rusqlite::params![status, note, now, id, from],
            )?,
        };
        if rows_affected == 0 {
            return Err(crate::error::LegionError::CardNotFound(id.to_string()));
        }

        // -- sync the bound document if the new status maps to a spec status --
        // Parse `status` into the typed enum so requirement_status_for_card_status
        // gets an exhaustive-match guarantee. An unrecognised status string is
        // treated as no-sync (same outcome as a None return from the mapping).
        let card_status_typed = std::str::FromStr::from_str(status).ok();
        if let Some(doc_id) = document_id
            && let Some(typed) = card_status_typed
            && let Some(spec_status) = Self::requirement_status_for_card_status(typed)
        {
            Self::sync_bound_document(&tx, doc_id, spec_status, &now)?;
        }

        tx.commit()?;
        Ok(())
    }

    /// Fetch, mutate, and persist a bound document's `meta.status` and
    /// hoisted `status` column to `spec_status`, inside the caller's
    /// transaction. Shared by `transition_card_status_with_sync` and
    /// `force_move_card` (#753) so both card-move paths keep a linked
    /// document's status in step identically.
    ///
    /// A missing document or an unparseable payload is a hard error --
    /// returning `Err` here propagates out of the caller's `?` before
    /// `tx.commit()`, so the whole transaction (including the card's own
    /// status UPDATE) rolls back rather than leaving the card and its spec
    /// out of sync.
    fn sync_bound_document(
        tx: &rusqlite::Transaction,
        doc_id: &str,
        spec_status: &str,
        now: &str,
    ) -> Result<()> {
        // Fetch and mutate the payload inside the transaction.
        // Borrowing `tx` as a read source is fine because rusqlite
        // allows reads mid-transaction on the same connection.
        let payload_str: Option<String> = {
            let mut stmt =
                tx.prepare("SELECT payload FROM documents WHERE id = ?1 AND deleted_at IS NULL")?;
            let mut rows = stmt.query(rusqlite::params![doc_id])?;
            rows.next()?
                .map(|row| row.get::<_, String>(0))
                .transpose()
                .map_err(crate::error::LegionError::Database)?
        };

        let payload_str = payload_str.ok_or_else(|| {
            crate::error::LegionError::WorkSource(format!(
                "bound document '{doc_id}' not found -- cannot sync status"
            ))
        })?;

        let mut value: serde_json::Value = serde_json::from_str(&payload_str).map_err(|e| {
            crate::error::LegionError::WorkSource(format!(
                "bound document '{doc_id}' has unparseable payload: {e}"
            ))
        })?;

        let meta = value
            .get_mut("meta")
            .and_then(|m| m.as_object_mut())
            .ok_or_else(|| {
                crate::error::LegionError::WorkSource(format!(
                    "bound document '{doc_id}' payload has no 'meta' object"
                ))
            })?;
        meta.insert(
            "status".to_string(),
            serde_json::Value::String(spec_status.to_string()),
        );

        let updated_payload = serde_json::to_string(&value)?;
        tx.execute(
            "UPDATE documents SET payload = ?1, status = ?2, updated_at = ?3 \
             WHERE id = ?4 AND deleted_at IS NULL",
            rusqlite::params![updated_payload, spec_status, now, doc_id],
        )?;
        Ok(())
    }

    /// Force-move a card to any status (bypasses state machine).
    ///
    /// Runs the same document-sync as the governed move path
    /// (`transition_card_status_with_sync`), inside one transaction with the
    /// card's own status/sort_order update (#753). A bound document that
    /// fails to sync (missing, or unparseable payload) rolls back the card
    /// move too -- a forced move must not leave the card and its linked
    /// document out of step.
    pub fn force_move_card(&self, id: &str, status: &str, sort_order: Option<i32>) -> Result<()> {
        let now = chrono::Utc::now().to_rfc3339();
        let sort = sort_order.unwrap_or(0);

        let tx = self.conn.unchecked_transaction()?;

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
            tx.execute(&sql, rusqlite::params![status, sort, now, id])?
        } else {
            tx.execute(&sql, rusqlite::params![status, sort, now, id, now])?
        };
        if rows == 0 {
            return Err(LegionError::CardNotFound(id.to_string()));
        }

        // -- sync the bound document, same as the governed move path --
        // Read document_id via `tx`, not a fresh connection read, so this
        // query is part of the same transaction as the UPDATE above --
        // isolated from any concurrent writer, and rolled back with the
        // card move if the sync below fails.
        let document_id: Option<String> = {
            let mut stmt =
                tx.prepare("SELECT document_id FROM tasks WHERE id = ?1 AND deleted_at IS NULL")?;
            let mut rows = stmt.query(rusqlite::params![id])?;
            rows.next()?
                .map(|row| row.get::<_, Option<String>>(0))
                .transpose()
                .map_err(LegionError::Database)?
                .flatten()
        };

        let card_status_typed = std::str::FromStr::from_str(status).ok();
        if let Some(doc_id) = document_id.as_deref()
            && let Some(typed) = card_status_typed
            && let Some(spec_status) = Self::requirement_status_for_card_status(typed)
        {
            Self::sync_bound_document(&tx, doc_id, spec_status, &now)?;
        }

        tx.commit()?;
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

    /// Return the id of the live (non-cancelled, non-deleted) card bound to
    /// `document_id`, or `None` when no such card exists.
    ///
    /// "Live" means `status != 'cancelled'` AND `deleted_at IS NULL`.
    ///
    /// `archive_document` calls this to enforce the guard. `bind_card_to_document`
    /// uses equivalent inline SQL inside its own transaction (it cannot call this
    /// method because `&self` and `&Transaction` are incompatible receivers);
    /// if the live definition changes, both sites must be kept in sync.
    pub fn live_card_bound_to_document(&self, document_id: &str) -> Result<Option<String>> {
        let mut stmt = self.conn.prepare(
            "SELECT id FROM tasks WHERE document_id = ?1 \
             AND deleted_at IS NULL \
             AND status != 'cancelled' \
             LIMIT 1",
        )?;
        let mut rows = stmt.query([document_id])?;
        match rows.next()? {
            Some(row) => Ok(Some(row.get(0)?)),
            None => Ok(None),
        }
    }

    /// Look up the live (non-cancelled, non-deleted) card whose `document_id`
    /// matches.
    ///
    /// Invariant: at most one live card is bound to a given document at any
    /// time (enforced by `bind_card_to_document`). The query adds
    /// `status != 'cancelled'` so a cancelled card that retains its
    /// `document_id` is never returned in place of a live successor.
    ///
    /// No production code path reaches this today; it is retained for the
    /// reverse-lookup AC from #512 and exercised by the binding tests (#528).
    #[allow(dead_code)]
    pub fn get_card_by_document_id(
        &self,
        document_id: &str,
    ) -> Result<Option<crate::kanban::Card>> {
        let sql = format!(
            "SELECT {} FROM tasks \
             WHERE document_id = ?1 AND deleted_at IS NULL AND status != 'cancelled' \
             LIMIT 1",
            Self::CARD_COLUMNS
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let mut rows = stmt.query_map([document_id], crate::kanban::map_card_row)?;
        match rows.next() {
            Some(row) => Ok(Some(row.map_err(LegionError::Database)?)),
            None => Ok(None),
        }
    }

    /// Bind a document to a card by setting `tasks.document_id`.
    ///
    /// All guards and the UPDATE run inside a single `unchecked_transaction`
    /// to prevent TOCTOU races: without the transaction two concurrent callers
    /// could both pass the live-card uniqueness check and then both write,
    /// leaving two live cards bound to the same document.
    ///
    /// Errors if:
    /// - The card does not exist or is deleted.
    /// - The card already has a `document_id` (already bound).
    /// - Another live (non-cancelled, non-deleted) card is already bound
    ///   to `document_id` (uniqueness guard).
    pub fn bind_card_to_document(&self, card_id: &str, document_id: &str) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;

        // Guard 1: card must exist and not already be bound.
        let card = {
            let sql = format!(
                "SELECT {} FROM tasks WHERE id = ?1 AND deleted_at IS NULL LIMIT 1",
                Self::CARD_COLUMNS
            );
            let mut stmt = tx.prepare(&sql)?;
            let mut rows = stmt.query_map([card_id], crate::kanban::map_card_row)?;
            match rows.next() {
                Some(row) => row.map_err(LegionError::Database)?,
                None => return Err(LegionError::CardNotFound(card_id.to_string())),
            }
        };
        if card.document_id.is_some() {
            return Err(LegionError::WorkSource(format!(
                "card '{card_id}' is already bound to document '{}'",
                card.document_id.as_deref().unwrap_or("")
            )));
        }

        // Guard 2: no other live card is bound to this document_id (inside tx).
        {
            let mut stmt = tx.prepare(
                "SELECT id FROM tasks WHERE document_id = ?1 \
                 AND deleted_at IS NULL \
                 AND status != 'cancelled' \
                 LIMIT 1",
            )?;
            let mut rows = stmt.query([document_id])?;
            if let Some(row) = rows.next()? {
                let existing_id: String = row.get(0)?;
                return Err(LegionError::WorkSource(format!(
                    "document '{document_id}' is already bound to live card '{existing_id}'"
                )));
            }
        }

        let now = chrono::Utc::now().to_rfc3339();
        let affected = tx.execute(
            "UPDATE tasks SET document_id = ?1, updated_at = ?2 \
             WHERE id = ?3 AND deleted_at IS NULL",
            rusqlite::params![document_id, now, card_id],
        )?;
        if affected == 0 {
            return Err(LegionError::CardNotFound(card_id.to_string()));
        }

        tx.commit()?;
        Ok(())
    }

    /// Per-agent workload summary.
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

    /// `PRIORITY_ORDER` is a SQL CASE over string literals; the Rust-side
    /// closed set is `kanban::Priority`. If a variant is added to the enum
    /// without a WHEN arm here (or vice versa), scheduler ordering silently
    /// drops to NULL for that priority -- this test pins the two together.
    #[test]
    fn priority_order_sql_covers_every_priority_variant() {
        use clap::ValueEnum;
        for p in crate::kanban::Priority::value_variants() {
            let arm = format!("WHEN '{p}' THEN");
            assert!(
                Database::PRIORITY_ORDER.contains(&arm),
                "PRIORITY_ORDER is missing an arm for priority '{p}'"
            );
        }
    }

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
                crate::kanban::Priority::Med,
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
                crate::kanban::Priority::Med,
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

    // --- document_id migration / binding tests (#528) ---

    fn insert_test_card(db: &Database) -> String {
        db.insert_card(
            "legion",
            "legion",
            "test card",
            None,
            crate::kanban::Priority::Med,
            None,
            None,
            None,
            None,
            None,
            crate::kanban::CardStatus::Backlog,
        )
        .expect("insert card")
    }

    fn insert_test_document(db: &Database) -> String {
        let meta = crate::documents::DocumentMeta {
            id: None,
            doc_type: "requirement",
            surface: Some("test"),
            status: Some("draft"),
            priority: None,
            owner: "legion",
        };
        let payload = serde_json::json!({
            "meta": {"id": "test", "type": "requirement", "surface": "test",
                     "status": "draft", "priority": "SHOULD", "owner": "legion",
                     "date": "2026-06-12", "author": "test"},
            "title": "Test Req",
            "description": "desc",
            "traces_to": "doc#mot.1",
            "depends_on": []
        })
        .to_string();
        db.insert_document(&meta, &payload)
            .expect("insert document")
            .id
    }

    /// Migration 16 is idempotent: opening the same file-backed database a
    /// second time must not fail even though the `document_id` column already
    /// exists. Uses a real file path (not in-memory) to prove the ALTER-if-
    /// missing guard fires correctly on re-open. The `TempDir` is kept alive
    /// for the duration of the test by binding it to a local variable.
    #[test]
    fn migration_document_id_column_is_idempotent_on_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test_idempotent.db");

        // First open: creates the schema including the document_id column.
        let db1 = Database::open(&db_path).expect("first open");
        let id = insert_test_card(&db1);
        let card = db1.get_card_by_id(&id).expect("get").expect("exists");
        assert!(
            card.document_id.is_none(),
            "new card has no document_id by default"
        );
        drop(db1);

        // Second open over the same path: migration 16's has_column guard must
        // prevent a duplicate ALTER TABLE from failing.
        let db2 = Database::open(&db_path).expect("second open must not fail");
        let card2 = db2.get_card_by_id(&id).expect("get").expect("exists");
        assert!(
            card2.document_id.is_none(),
            "card from first open readable after second open"
        );
        // dir is dropped here, cleaning up the temp files.
    }

    /// Bind happy path: bind_card_to_document sets document_id on the card.
    #[test]
    fn bind_card_to_document_happy_path() {
        let db = test_db();
        let card_id = insert_test_card(&db);
        let doc_id = insert_test_document(&db);

        db.bind_card_to_document(&card_id, &doc_id).expect("bind");

        let card = db.get_card_by_id(&card_id).expect("get").expect("exists");
        assert_eq!(
            card.document_id.as_deref(),
            Some(doc_id.as_str()),
            "document_id set on card after bind"
        );

        // Reverse lookup: get_card_by_document_id returns the bound card.
        let found = db
            .get_card_by_document_id(&doc_id)
            .expect("lookup")
            .expect("found");
        assert_eq!(found.id, card_id);
    }

    /// Error: card already has a document_id.
    #[test]
    fn bind_fails_when_card_already_bound() {
        let db = test_db();
        let card_id = insert_test_card(&db);
        let doc_id1 = insert_test_document(&db);
        let doc_id2 = insert_test_document(&db);

        db.bind_card_to_document(&card_id, &doc_id1)
            .expect("first bind");
        let err = db.bind_card_to_document(&card_id, &doc_id2).unwrap_err();
        assert!(
            err.to_string().contains("already bound"),
            "expected already-bound error, got: {err}"
        );
    }

    /// Error: document already bound to another live card.
    #[test]
    fn bind_fails_when_document_already_bound_to_live_card() {
        let db = test_db();
        let card_id1 = insert_test_card(&db);
        let card_id2 = insert_test_card(&db);
        let doc_id = insert_test_document(&db);

        db.bind_card_to_document(&card_id1, &doc_id)
            .expect("first bind");
        let err = db.bind_card_to_document(&card_id2, &doc_id).unwrap_err();
        assert!(
            err.to_string().contains("already bound to live card"),
            "expected already-bound-to-live-card error, got: {err}"
        );
    }

    /// Error: card not found.
    #[test]
    fn bind_fails_when_card_not_found() {
        let db = test_db();
        let doc_id = insert_test_document(&db);
        let err = db
            .bind_card_to_document("nonexistent-id", &doc_id)
            .unwrap_err();
        assert!(
            matches!(err, LegionError::CardNotFound(_)),
            "expected CardNotFound, got: {err}"
        );
    }

    /// Binding a document to a cancelled card is allowed in bind_card_to_document
    /// (the uniqueness guard only blocks live non-cancelled cards). This tests
    /// that a cancelled card's document_id slot does NOT block a new card from
    /// binding to the same doc.
    #[test]
    fn bind_allows_rebind_after_card_cancelled() {
        let db = test_db();
        let card_id1 = insert_test_card(&db);
        let card_id2 = insert_test_card(&db);
        let doc_id = insert_test_document(&db);

        // Bind to card1, then cancel card1.
        db.bind_card_to_document(&card_id1, &doc_id)
            .expect("bind to card1");
        // Cancel by force_move (bypasses state machine for test simplicity).
        db.force_move_card(&card_id1, "cancelled", None)
            .expect("cancel card1");

        // Now card2 can bind to the same doc (card1 is cancelled, not live).
        db.bind_card_to_document(&card_id2, &doc_id)
            .expect("rebind to card2 after cancel");
        let card2 = db.get_card_by_id(&card_id2).expect("get").expect("exists");
        assert_eq!(card2.document_id.as_deref(), Some(doc_id.as_str()));
    }

    /// get_card_by_document_id returns the LIVE card when both a cancelled and
    /// a live card share the same document_id. The cancelled card must not
    /// shadow the live one.
    #[test]
    fn get_card_by_document_id_returns_live_card_not_cancelled() {
        let db = test_db();
        let card_id1 = insert_test_card(&db);
        let card_id2 = insert_test_card(&db);
        let doc_id = insert_test_document(&db);

        // Bind to card1, cancel it, then bind card2 to the same doc.
        db.bind_card_to_document(&card_id1, &doc_id)
            .expect("bind card1");
        db.force_move_card(&card_id1, "cancelled", None)
            .expect("cancel card1");
        db.bind_card_to_document(&card_id2, &doc_id)
            .expect("bind card2");

        // Reverse lookup must return card2 (live), not card1 (cancelled).
        let found = db
            .get_card_by_document_id(&doc_id)
            .expect("lookup")
            .expect("some card found");
        assert_eq!(
            found.id, card_id2,
            "get_card_by_document_id must return the live card, not the cancelled one"
        );
    }

    /// requirement_status_for_card_status maps the four expected statuses.
    /// Uses typed CardStatus so adding a new variant forces a compile-time
    /// decision in the match (no catch-all).
    #[test]
    fn requirement_status_mapping_covers_all_mapped_statuses() {
        use crate::kanban::CardStatus;
        assert_eq!(
            Database::requirement_status_for_card_status(CardStatus::Accepted),
            Some("accepted")
        );
        assert_eq!(
            Database::requirement_status_for_card_status(CardStatus::InReview),
            Some("implemented")
        );
        assert_eq!(
            Database::requirement_status_for_card_status(CardStatus::Done),
            Some("verified")
        );
        assert_eq!(
            Database::requirement_status_for_card_status(CardStatus::Cancelled),
            Some("cancelled")
        );
        // Statuses that carry no spec meaning return None.
        assert!(Database::requirement_status_for_card_status(CardStatus::Backlog).is_none());
        assert!(Database::requirement_status_for_card_status(CardStatus::Pending).is_none());
        assert!(Database::requirement_status_for_card_status(CardStatus::NeedsInput).is_none());
        assert!(Database::requirement_status_for_card_status(CardStatus::Blocked).is_none());
        assert!(Database::requirement_status_for_card_status(CardStatus::Delegated).is_none());
        assert!(Database::requirement_status_for_card_status(CardStatus::Deferred).is_none());
    }

    // -- get_delegated_cards (#778) -------------------------------------------

    #[test]
    fn get_delegated_cards_returns_only_delegated_status() {
        let db = test_db();
        let delegated_id = db
            .insert_card(
                "legion",
                "legion",
                "delegated card",
                None,
                crate::kanban::Priority::Med,
                None,
                None,
                None,
                None,
                None,
                crate::kanban::CardStatus::Delegated,
            )
            .unwrap();
        db.insert_card(
            "legion",
            "legion",
            "accepted card",
            None,
            crate::kanban::Priority::Med,
            None,
            None,
            None,
            None,
            None,
            crate::kanban::CardStatus::Accepted,
        )
        .unwrap();

        let delegated = db.get_delegated_cards(None).unwrap();
        assert_eq!(delegated.len(), 1);
        assert_eq!(delegated[0].id, delegated_id);
    }

    #[test]
    fn get_delegated_cards_filters_by_repo_when_given() {
        let db = test_db();
        let legion_id = db
            .insert_card(
                "legion",
                "legion",
                "delegated in legion",
                None,
                crate::kanban::Priority::Med,
                None,
                None,
                None,
                None,
                None,
                crate::kanban::CardStatus::Delegated,
            )
            .unwrap();
        db.insert_card(
            "legion",
            "huttspawn",
            "delegated in huttspawn",
            None,
            crate::kanban::Priority::Med,
            None,
            None,
            None,
            None,
            None,
            crate::kanban::CardStatus::Delegated,
        )
        .unwrap();

        let scoped = db.get_delegated_cards(Some("legion")).unwrap();
        assert_eq!(scoped.len(), 1);
        assert_eq!(scoped[0].id, legion_id);

        let all = db.get_delegated_cards(None).unwrap();
        assert_eq!(all.len(), 2, "None scans every repo");
    }

    // -- kanban defer (#816) --------------------------------------------------

    #[test]
    fn migration_wake_at_and_pre_defer_status_columns_are_idempotent_on_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test_defer_idempotent.db");

        let db1 = Database::open(&db_path).expect("first open");
        let id = insert_test_card(&db1);
        let card = db1.get_card_by_id(&id).expect("get").expect("exists");
        assert!(card.wake_at.is_none());
        assert!(card.pre_defer_status.is_none());
        drop(db1);

        // Second open must not fail even though both columns already exist.
        let db2 = Database::open(&db_path).expect("second open must not fail");
        let card2 = db2.get_card_by_id(&id).expect("get").expect("exists");
        assert!(card2.wake_at.is_none());
    }

    #[test]
    fn set_card_deferred_and_clear_defer_fields_round_trip() {
        let db = test_db();
        let id = insert_test_card(&db);

        db.set_card_deferred(&id, None, "2099-01-01T00:00:00+00:00", "accepted")
            .expect("set deferred");
        let card = db.get_card_by_id(&id).expect("get").expect("exists");
        assert_eq!(card.status.to_string(), "deferred");
        assert_eq!(card.wake_at.as_deref(), Some("2099-01-01T00:00:00+00:00"));
        assert_eq!(card.pre_defer_status.as_deref(), Some("accepted"));

        db.clear_card_defer_fields(&id).expect("clear defer fields");
        let card = db.get_card_by_id(&id).expect("get").expect("exists");
        assert!(card.wake_at.is_none());
        assert!(card.pre_defer_status.is_none());
    }

    #[test]
    fn set_card_deferred_sets_note_and_reports_not_found() {
        let db = test_db();
        let id = insert_test_card(&db);

        db.set_card_deferred(
            &id,
            Some("deferred for now"),
            "2099-01-01T00:00:00+00:00",
            "accepted",
        )
        .expect("set deferred with note");
        let card = db.get_card_by_id(&id).expect("get").expect("exists");
        assert_eq!(card.note.as_deref(), Some("deferred for now"));

        let err = db
            .set_card_deferred(
                "nonexistent-id",
                None,
                "2099-01-01T00:00:00+00:00",
                "accepted",
            )
            .unwrap_err();
        assert!(matches!(err, LegionError::CardNotFound(_)));
    }

    #[test]
    fn get_deferred_cards_due_only_returns_deferred_with_past_wake_at() {
        let db = test_db();

        // Due: deferred, wake_at in the past.
        let due_id = db
            .insert_card(
                "legion",
                "legion",
                "due for wake",
                None,
                crate::kanban::Priority::Med,
                None,
                None,
                None,
                None,
                None,
                crate::kanban::CardStatus::Deferred,
            )
            .unwrap();
        db.set_card_deferred(&due_id, None, "2020-01-01T00:00:00+00:00", "accepted")
            .unwrap();

        // Not due: deferred, wake_at in the future.
        let future_id = db
            .insert_card(
                "legion",
                "legion",
                "not due yet",
                None,
                crate::kanban::Priority::Med,
                None,
                None,
                None,
                None,
                None,
                crate::kanban::CardStatus::Deferred,
            )
            .unwrap();
        db.set_card_deferred(&future_id, None, "2099-01-01T00:00:00+00:00", "accepted")
            .unwrap();

        // Not deferred at all -- must never appear regardless of wake_at.
        db.insert_card(
            "legion",
            "legion",
            "plain accepted card",
            None,
            crate::kanban::Priority::Med,
            None,
            None,
            None,
            None,
            None,
            crate::kanban::CardStatus::Accepted,
        )
        .unwrap();

        let now = chrono::Utc::now().to_rfc3339();
        let due = db.get_deferred_cards_due(&now).unwrap();
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].id, due_id);
    }

    #[test]
    fn get_cards_working_set_excludes_deferred() {
        let db = test_db();
        let deferred_id = db
            .insert_card(
                "legion",
                "legion",
                "deferred card",
                None,
                crate::kanban::Priority::Med,
                None,
                None,
                None,
                None,
                None,
                crate::kanban::CardStatus::Deferred,
            )
            .unwrap();
        db.insert_card(
            "legion",
            "legion",
            "accepted card",
            None,
            crate::kanban::Priority::Med,
            None,
            None,
            None,
            None,
            None,
            crate::kanban::CardStatus::Accepted,
        )
        .unwrap();

        let working_set = db
            .get_cards(
                "legion",
                crate::kanban::Direction::Inbound,
                crate::kanban::CardScope::WorkingSet,
            )
            .unwrap();
        assert_eq!(working_set.len(), 1, "WorkingSet must exclude Deferred");
        assert!(working_set.iter().all(|c| c.id != deferred_id));

        // But it must still be visible via the dedicated Deferred scope --
        // never silently uncounted (#798's lesson applied to a new status).
        let deferred_scope = db
            .get_cards(
                "legion",
                crate::kanban::Direction::Inbound,
                crate::kanban::CardScope::Deferred,
            )
            .unwrap();
        assert_eq!(deferred_scope.len(), 1);
        assert_eq!(deferred_scope[0].id, deferred_id);
    }

    /// Transactional sync: transitioning a bound card to "done" updates BOTH
    /// tasks.status AND documents.status + documents.payload in one commit.
    #[test]
    fn transition_with_sync_updates_both_card_and_document() {
        let db = test_db();
        let card_id = insert_test_card(&db);
        let doc_id = insert_test_document(&db);

        // Bind and move the card to a state where Done is reachable.
        db.bind_card_to_document(&card_id, &doc_id).expect("bind");
        db.force_move_card(&card_id, "accepted", None)
            .expect("move to accepted");

        // Transition to Done -- syncs the doc to "verified".
        db.transition_card_status_with_sync(
            &card_id,
            "done",
            None,
            CardTimestamp::Completed,
            Some(&doc_id),
            None,
        )
        .expect("transition");

        // Card must be done.
        let card = db.get_card_by_id(&card_id).expect("get").expect("exists");
        assert_eq!(card.status.to_string(), "done");

        // Document status column must be "verified".
        let doc = db
            .get_document(&doc_id)
            .expect("get doc")
            .expect("doc exists");
        assert_eq!(
            doc.status, "verified",
            "hoisted status column must be 'verified'"
        );

        // Document payload meta.status must also be "verified".
        let payload: serde_json::Value = serde_json::from_str(&doc.payload).expect("parse payload");
        assert_eq!(
            payload["meta"]["status"].as_str(),
            Some("verified"),
            "payload meta.status must be 'verified'"
        );
    }

    /// Transactional sync: cancelled card -> doc status "cancelled".
    #[test]
    fn done_to_verified_and_cancelled_to_cancelled_mapping() {
        let db = test_db();
        let card_id = insert_test_card(&db);
        let doc_id = insert_test_document(&db);

        db.bind_card_to_document(&card_id, &doc_id).expect("bind");
        // Force to accepted so Cancel is valid from the state machine perspective
        db.force_move_card(&card_id, "accepted", None)
            .expect("move");

        db.transition_card_status_with_sync(
            &card_id,
            "cancelled",
            None,
            CardTimestamp::Completed,
            Some(&doc_id),
            None,
        )
        .expect("cancel transition");

        let doc = db.get_document(&doc_id).expect("get").expect("exists");
        assert_eq!(doc.status, "cancelled");
        let payload: serde_json::Value = serde_json::from_str(&doc.payload).expect("parse");
        assert_eq!(payload["meta"]["status"].as_str(), Some("cancelled"));
    }

    /// Illegal transition is rolled back -- neither card nor doc changes.
    #[test]
    fn illegal_transition_rolls_back_both_card_and_doc() {
        use crate::kanban::{Action, CardStatus, transition_card};
        let db = test_db();
        let card_id = insert_test_card(&db);
        let doc_id = insert_test_document(&db);

        db.bind_card_to_document(&card_id, &doc_id).expect("bind");
        // Card is in Backlog -- Done is not a valid action from Backlog.
        let err = transition_card(&db, &card_id, Action::Done, None).unwrap_err();
        assert!(
            matches!(err, LegionError::InvalidCardTransition { .. }),
            "expected InvalidCardTransition, got: {err}"
        );

        // Card must still be in Backlog.
        let card = db.get_card_by_id(&card_id).expect("get").expect("exists");
        assert_eq!(card.status, CardStatus::Backlog);

        // Document must still be "draft".
        let doc = db.get_document(&doc_id).expect("get").expect("exists");
        assert_eq!(doc.status, "draft");
    }

    /// Unparseable bound document payload fails the transition.
    #[test]
    fn unparseable_bound_payload_fails_transition() {
        let db = test_db();
        let card_id = insert_test_card(&db);

        // Insert a document with an invalid (non-JSON) payload.
        let meta = crate::documents::DocumentMeta {
            id: None,
            doc_type: "requirement",
            surface: Some("test"),
            status: Some("draft"),
            priority: None,
            owner: "legion",
        };
        let doc_id = db
            .insert_document(&meta, "NOT VALID JSON")
            .expect("insert corrupt doc")
            .id;
        db.bind_card_to_document(&card_id, &doc_id).expect("bind");
        // No status-changing setup call here: any mapped status (including
        // "accepted") would already hit this same corrupt payload via
        // force_move_card's own doc-sync (#753). The card stays at its
        // as-inserted "backlog" status until the transition below.

        // Transitioning to "done" should fail because the payload is unparseable.
        let err = db
            .transition_card_status_with_sync(
                &card_id,
                "done",
                None,
                CardTimestamp::Completed,
                Some(&doc_id),
                None,
            )
            .unwrap_err();
        assert!(
            err.to_string().contains("unparseable payload"),
            "expected unparseable payload error, got: {err}"
        );

        // Card must NOT have changed status (transaction rolled back).
        let card = db.get_card_by_id(&card_id).expect("get").expect("exists");
        assert_eq!(
            card.status.to_string(),
            "backlog",
            "card status must not change"
        );

        // Document status must also be unchanged (both sides of the tx rolled back).
        let doc = db.get_document(&doc_id).expect("get doc").expect("exists");
        assert_eq!(
            doc.status, "draft",
            "document status must remain 'draft' when transition fails"
        );
    }

    /// Status sync does NOT fire for statuses with no spec mapping (e.g. "blocked").
    #[test]
    fn transition_without_mapped_status_does_not_touch_document() {
        let db = test_db();
        let card_id = insert_test_card(&db);
        let doc_id = insert_test_document(&db);

        db.bind_card_to_document(&card_id, &doc_id).expect("bind");
        // No status-changing setup call here: force_move_card now runs the
        // same doc-sync as the governed path (#753), and "accepted" is
        // itself a mapped status, so moving through it would already flip
        // the doc away from "draft" before this test's own assertion.

        // blocked has no spec mapping -- doc stays "draft".
        db.transition_card_status_with_sync(
            &card_id,
            "blocked",
            None,
            CardTimestamp::None,
            Some(&doc_id),
            None,
        )
        .expect("transition to blocked");

        let doc = db.get_document(&doc_id).expect("get").expect("exists");
        assert_eq!(
            doc.status, "draft",
            "document status must be unchanged for unmapped card status"
        );
    }

    /// pick_next_card on a bound card must update BOTH tasks.status and the
    /// spec document's status column + payload meta.status. Previously the
    /// direct UPDATE in pick_next_card bypassed transition_card_status_with_sync
    /// entirely, leaving the document stale (#528 HIGH finding).
    #[test]
    fn pick_next_card_syncs_bound_document_status() {
        let db = test_db();
        let card_id = insert_test_card(&db);
        let doc_id = insert_test_document(&db);

        // Bind the spec and move the card to pending (pick_next_card selects pending).
        db.bind_card_to_document(&card_id, &doc_id).expect("bind");
        db.force_move_card(&card_id, "pending", None)
            .expect("move to pending");

        // pick_next_card accepts the card -- must also update the spec to "accepted".
        let picked = db
            .pick_next_card("legion")
            .expect("pick")
            .expect("card returned");
        assert_eq!(picked.id, card_id);
        assert_eq!(picked.status, crate::kanban::CardStatus::Accepted);

        // Spec document status column must be "accepted".
        let doc = db.get_document(&doc_id).expect("get doc").expect("exists");
        assert_eq!(
            doc.status, "accepted",
            "spec document status column must be updated by pick_next_card"
        );

        // Spec payload meta.status must also be "accepted".
        let payload: serde_json::Value = serde_json::from_str(&doc.payload).expect("parse payload");
        assert_eq!(
            payload["meta"]["status"].as_str(),
            Some("accepted"),
            "spec payload meta.status must be updated by pick_next_card"
        );
    }

    /// force_move_card on a bound card must run the same document-sync as the
    /// governed move path: forcing a move to "done" syncs the linked
    /// document's hoisted status column AND payload meta.status to
    /// "verified" in the same commit as the card's own status update (#753).
    #[test]
    fn force_move_card_syncs_bound_document_status() {
        let db = test_db();
        let card_id = insert_test_card(&db);
        let doc_id = insert_test_document(&db);

        db.bind_card_to_document(&card_id, &doc_id).expect("bind");

        // Force straight to "done" -- a forced move bypasses the state
        // machine, so this single call is the whole test.
        db.force_move_card(&card_id, "done", None)
            .expect("force move to done");

        let card = db.get_card_by_id(&card_id).expect("get").expect("exists");
        assert_eq!(card.status.to_string(), "done");

        let doc = db
            .get_document(&doc_id)
            .expect("get doc")
            .expect("doc exists");
        assert_eq!(
            doc.status, "verified",
            "hoisted status column must follow the forced card move"
        );

        let payload: serde_json::Value = serde_json::from_str(&doc.payload).expect("parse payload");
        assert_eq!(
            payload["meta"]["status"].as_str(),
            Some("verified"),
            "payload meta.status must follow the forced card move"
        );
    }

    /// force_move_card doc-sync failure (unparseable bound document payload)
    /// rolls back the card move too -- a forced move must not leave the card
    /// and its linked document out of step (#753).
    #[test]
    fn force_move_card_rolls_back_on_doc_sync_failure() {
        let db = test_db();
        let card_id = insert_test_card(&db);

        // Insert a document with an invalid (non-JSON) payload.
        let meta = crate::documents::DocumentMeta {
            id: None,
            doc_type: "requirement",
            surface: Some("test"),
            status: Some("draft"),
            priority: None,
            owner: "legion",
        };
        let doc_id = db
            .insert_document(&meta, "NOT VALID JSON")
            .expect("insert corrupt doc")
            .id;
        db.bind_card_to_document(&card_id, &doc_id).expect("bind");

        // Forcing the card to "done" triggers a doc-sync (done -> verified)
        // that must fail because the payload is unparseable.
        let err = db.force_move_card(&card_id, "done", None).unwrap_err();
        assert!(
            err.to_string().contains("unparseable payload"),
            "expected unparseable payload error, got: {err}"
        );

        // Card must NOT have moved (whole transaction rolled back).
        let card = db.get_card_by_id(&card_id).expect("get").expect("exists");
        assert_eq!(
            card.status,
            crate::kanban::CardStatus::Backlog,
            "card status must not change when doc-sync fails"
        );

        // Document status must also be unchanged.
        let doc = db.get_document(&doc_id).expect("get doc").expect("exists");
        assert_eq!(
            doc.status, "draft",
            "document status must remain 'draft' when the forced move fails"
        );
    }

    /// force_move_card on a card with NO linked document is unaffected by the
    /// #753 doc-sync change: same status/sort_order/timestamp behavior as
    /// before, no error, no document touched (there being none).
    #[test]
    fn force_move_card_unbound_card_unchanged_behavior() {
        let db = test_db();
        let card_id = insert_test_card(&db);

        db.force_move_card(&card_id, "done", Some(7))
            .expect("force move unbound card");

        let card = db.get_card_by_id(&card_id).expect("get").expect("exists");
        assert_eq!(card.status.to_string(), "done");
        assert_eq!(card.sort_order, 7);
        assert!(
            card.completed_at.is_some(),
            "done timestamp must still be set for unbound cards"
        );
    }

    /// Concurrent-pick loser returns Ok(None). Simulated by calling
    /// transition_card_status_with_sync directly with expected_from_status =
    /// Some("pending") after the card is already accepted -- the AND status =
    /// 'pending' predicate finds no matching row (rows_affected == 0 ->
    /// CardNotFound), and pick_next_card maps that to Ok(None).
    ///
    /// This pins the guard that pick_next_card relies on: two concurrent callers
    /// race to transition the same card; exactly one wins, the other gets None.
    #[test]
    fn pick_next_card_race_loser_returns_none() {
        let db = test_db();
        let card_id = insert_test_card(&db);

        // Move the card to pending so the first pick succeeds.
        db.force_move_card(&card_id, "pending", None)
            .expect("move to pending");

        // First pick: succeeds and transitions the card to accepted.
        let first = db.pick_next_card("legion").expect("first pick");
        assert!(first.is_some(), "first pick must succeed");
        assert_eq!(
            first.unwrap().status,
            crate::kanban::CardStatus::Accepted,
            "first pick must set status to accepted"
        );

        // Simulate the second picker: it read the card when it was still pending,
        // but by the time it writes, the card is accepted. Call the inner path
        // directly with expected_from_status = Some("pending") -- the predicate
        // now fails because status = 'accepted', so rows_affected == 0 ->
        // CardNotFound, which pick_next_card maps to Ok(None).
        let race_result = db.transition_card_status_with_sync(
            &card_id,
            &crate::kanban::CardStatus::Accepted.to_string(),
            None,
            CardTimestamp::Started,
            None,
            Some("pending"), // stale expectation: card is already accepted
        );
        assert!(
            matches!(race_result, Err(LegionError::CardNotFound(_))),
            "stale expected_from_status must yield CardNotFound, got: {race_result:?}"
        );
    }
}
