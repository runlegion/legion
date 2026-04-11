use std::path::Path;

use chrono::{Timelike, Utc};
use rusqlite::{Connection, OptionalExtension};
use uuid::Uuid;

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

/// A reflection row returned with its embedding blob for dedupe checks.
///
/// Tuple fields: (id, embedding_bytes, text, created_at).
pub type ReflectionWithEmbedding = (String, Vec<u8>, String, String);

/// Which timestamp column to set during a card status update.
#[derive(Debug, Clone, Copy)]
#[allow(dead_code)]
pub enum CardTimestamp {
    Assigned,
    Started,
    Completed,
    None,
}

/// Persistent storage for reflections backed by SQLite.
pub struct Database {
    conn: Connection,
}

/// A single stored reflection tied to a repository.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Reflection {
    pub id: String,
    pub repo: String,
    pub text: String,
    pub created_at: String,
    pub audience: String,
    // Phase 2.0: Synapse metadata
    pub domain: Option<String>,
    pub tags: Option<String>,
    pub recall_count: i64,
    pub last_recalled_at: Option<String>,
    pub parent_id: Option<String>,
}

/// Per-repo dashboard stats for the serve API.
#[derive(Debug, Clone, serde::Serialize)]
pub struct DashboardRepoStats {
    pub repo: String,
    pub reflection_count: u64,
    pub boost_sum: i64,
    pub team_post_count: u64,
    pub last_activity: String,
}

/// Aggregate statistics for a repository's reflections.
#[derive(Debug)]
pub struct RepoStats {
    pub repo: String,
    pub count: u64,
    pub oldest: String,
    pub newest: String,
}

/// Map a database row to a Reflection struct.
///
/// Shared by all queries that select
/// (id, repo, text, created_at, audience, domain, tags, recall_count,
///  last_recalled_at, parent_id).
fn map_reflection_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Reflection> {
    Ok(Reflection {
        id: row.get(0)?,
        repo: row.get(1)?,
        text: row.get(2)?,
        created_at: row.get(3)?,
        audience: row.get(4)?,
        domain: row.get(5)?,
        tags: row.get(6)?,
        recall_count: row.get(7)?,
        last_recalled_at: row.get(8)?,
        parent_id: row.get(9)?,
    })
}

/// Map a database row to a Schedule struct.
fn map_schedule_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Schedule> {
    let enabled_int: i32 = row.get(5)?;
    Ok(Schedule {
        id: row.get(0)?,
        name: row.get(1)?,
        cron: row.get(2)?,
        command: row.get(3)?,
        repo: row.get(4)?,
        enabled: enabled_int != 0,
        last_run: row.get(6)?,
        next_run: row.get(7)?,
        created_at: row.get(8)?,
        active_start: row.get(9)?,
        active_end: row.get(10)?,
    })
}

/// Optional metadata for a new reflection (Phase 2.0 Synapse fields).
#[derive(Default)]
pub struct ReflectionMeta {
    pub domain: Option<String>,
    pub tags: Option<String>,
    pub parent_id: Option<String>,
}

/// A scheduled command that fires on a cron-like schedule.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Schedule {
    pub id: String,
    pub name: String,
    pub cron: String,
    pub command: String,
    pub repo: String,
    pub enabled: bool,
    pub last_run: Option<String>,
    pub next_run: String,
    pub created_at: String,
    pub active_start: Option<String>,
    pub active_end: Option<String>,
}

/// Parse an HH:MM string into minutes since midnight. Returns None if invalid.
fn parse_hhmm(s: &str) -> Option<u32> {
    let parts: Vec<&str> = s.split(':').collect();
    if parts.len() != 2 {
        return None;
    }
    let h: u32 = parts[0].parse().ok()?;
    let m: u32 = parts[1].parse().ok()?;
    if h >= 24 || m >= 60 {
        return None;
    }
    Some(h * 60 + m)
}

/// Validate an HH:MM time string. Returns an error with a descriptive message if invalid.
pub fn validate_hhmm(s: &str) -> Result<()> {
    if parse_hhmm(s).is_none() {
        return Err(LegionError::InvalidCron(format!(
            "invalid time format '{s}': expected HH:MM with hours 0-23 and minutes 0-59"
        )));
    }
    Ok(())
}

/// Check if a schedule is within its active time window.
/// Handles overnight windows (e.g., 23:00-07:00 crosses midnight).
/// Schedules without a window are always active.
fn is_in_active_window(schedule: &Schedule, now: &chrono::DateTime<Utc>) -> bool {
    let (start_str, end_str) = match (&schedule.active_start, &schedule.active_end) {
        (Some(s), Some(e)) => (s.as_str(), e.as_str()),
        _ => return true,
    };

    let start_minutes: u32 = match parse_hhmm(start_str) {
        Some(v) => v,
        None => return true,
    };
    let end_minutes: u32 = match parse_hhmm(end_str) {
        Some(v) => v,
        None => return true,
    };

    let now_minutes: u32 = now.hour() * 60 + now.minute();

    if start_minutes <= end_minutes {
        now_minutes >= start_minutes && now_minutes < end_minutes
    } else {
        now_minutes >= start_minutes || now_minutes < end_minutes
    }
}

/// Parse a simple cron expression and compute the next run time from `now`.
///
/// Supported formats:
/// - `HH:MM` -- daily at that time (UTC)
/// - `*/Nm` -- every N minutes from now
pub fn compute_next_run(cron: &str, now: chrono::DateTime<Utc>) -> Result<chrono::DateTime<Utc>> {
    if let Some(stripped) = cron.strip_prefix("*/") {
        // Interval format: */Nm
        let minutes_str = stripped
            .strip_suffix('m')
            .ok_or_else(|| LegionError::InvalidCron(cron.to_string()))?;
        let minutes: i64 = minutes_str
            .parse()
            .map_err(|_| LegionError::InvalidCron(cron.to_string()))?;
        if minutes <= 0 {
            return Err(LegionError::InvalidCron(cron.to_string()));
        }
        Ok(now + chrono::Duration::minutes(minutes))
    } else {
        // Daily format: HH:MM
        let parts: Vec<&str> = cron.split(':').collect();
        if parts.len() != 2 {
            return Err(LegionError::InvalidCron(cron.to_string()));
        }
        let hour: u32 = parts[0]
            .parse()
            .map_err(|_| LegionError::InvalidCron(cron.to_string()))?;
        let minute: u32 = parts[1]
            .parse()
            .map_err(|_| LegionError::InvalidCron(cron.to_string()))?;
        if hour >= 24 || minute >= 60 {
            return Err(LegionError::InvalidCron(cron.to_string()));
        }

        let today = now
            .date_naive()
            .and_hms_opt(hour, minute, 0)
            .ok_or_else(|| LegionError::InvalidCron(cron.to_string()))?;
        let today_utc = today.and_utc();

        if today_utc > now {
            Ok(today_utc)
        } else {
            // Tomorrow at that time
            let tomorrow = today_utc + chrono::Duration::days(1);
            Ok(tomorrow)
        }
    }
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

    /// Create the reflections table, indexes, and supporting tables.
    ///
    /// Uses `has_column` checks to skip already-applied migrations, so
    /// on a fully-migrated database this does minimal work (CREATE IF NOT
    /// EXISTS checks and a single PRAGMA query).
    fn init_schema(conn: &Connection) -> Result<()> {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS reflections (
                id TEXT PRIMARY KEY,
                repo TEXT NOT NULL,
                text TEXT NOT NULL,
                created_at TEXT NOT NULL,
                embedding BLOB
            );
            CREATE INDEX IF NOT EXISTS idx_reflections_repo ON reflections(repo);
            CREATE INDEX IF NOT EXISTS idx_reflections_created ON reflections(created_at);",
        )?;

        // Migration 1: add audience column + board_reads table.
        // Only run when the column does not yet exist.
        if !Self::has_column(conn, "reflections", "audience")? {
            conn.execute_batch(
                "ALTER TABLE reflections ADD COLUMN audience TEXT NOT NULL DEFAULT 'self';",
            )?;
        }

        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS board_reads (
                reader_repo TEXT NOT NULL PRIMARY KEY,
                last_read_at TEXT NOT NULL
            );",
        )?;

        // Migration 2: Phase 2.0 Synapse metadata columns.
        if !Self::has_column(conn, "reflections", "domain")? {
            conn.execute_batch("ALTER TABLE reflections ADD COLUMN domain TEXT;")?;
        }
        if !Self::has_column(conn, "reflections", "tags")? {
            conn.execute_batch("ALTER TABLE reflections ADD COLUMN tags TEXT;")?;
        }
        if !Self::has_column(conn, "reflections", "recall_count")? {
            conn.execute_batch(
                "ALTER TABLE reflections ADD COLUMN recall_count INTEGER NOT NULL DEFAULT 0;",
            )?;
        }
        if !Self::has_column(conn, "reflections", "last_recalled_at")? {
            conn.execute_batch("ALTER TABLE reflections ADD COLUMN last_recalled_at TEXT;")?;
        }
        if !Self::has_column(conn, "reflections", "parent_id")? {
            conn.execute_batch("ALTER TABLE reflections ADD COLUMN parent_id TEXT;")?;
        }

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

        // Migration 4: Schedules table for cron-like scheduled posts.
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS schedules (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                cron TEXT NOT NULL,
                command TEXT NOT NULL,
                repo TEXT NOT NULL,
                enabled INTEGER NOT NULL DEFAULT 1,
                last_run TEXT,
                next_run TEXT NOT NULL,
                created_at TEXT NOT NULL
            );",
        )?;

        // Migration 5: Add time window columns to schedules.
        if !Self::has_column(conn, "schedules", "active_start")? {
            conn.execute_batch("ALTER TABLE schedules ADD COLUMN active_start TEXT;")?;
        }
        if !Self::has_column(conn, "schedules", "active_end")? {
            conn.execute_batch("ALTER TABLE schedules ADD COLUMN active_end TEXT;")?;
        }

        // Migration 6: Add handled_at column for watch auto-wake tracking.
        if !Self::has_column(conn, "reflections", "handled_at")? {
            conn.execute_batch("ALTER TABLE reflections ADD COLUMN handled_at TEXT;")?;
        }

        // Migration 7: Per-repo signal handling for @all broadcasts (#85).
        // The watch_handled table tracks which repo has seen which signal,
        // replacing the global handled_at column for watch purposes.
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS watch_handled (
                signal_id TEXT NOT NULL,
                repo_name TEXT NOT NULL,
                handled_at TEXT NOT NULL,
                PRIMARY KEY (signal_id, repo_name)
            );",
        )?;

        // Migration 8: Health samples table for system telemetry (#88).
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS health_samples (
                id TEXT PRIMARY KEY,
                hostname TEXT NOT NULL,
                sampled_at TEXT NOT NULL,
                cpu_usage_pct REAL NOT NULL,
                load_avg_1 REAL,
                load_avg_5 REAL,
                load_avg_15 REAL,
                cpu_core_count INTEGER NOT NULL,
                mem_total_bytes INTEGER NOT NULL,
                mem_used_bytes INTEGER NOT NULL,
                mem_usage_pct REAL NOT NULL,
                swap_total_bytes INTEGER,
                swap_used_bytes INTEGER,
                cpu_temp_celsius REAL,
                agents_active INTEGER NOT NULL DEFAULT 0,
                pressure REAL NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_health_hostname ON health_samples(hostname);
            CREATE INDEX IF NOT EXISTS idx_health_sampled ON health_samples(sampled_at);",
        )?;

        // Migration 9: Kanban upgrade -- new columns on tasks table.
        if !Self::has_column(conn, "tasks", "labels")? {
            conn.execute_batch("ALTER TABLE tasks ADD COLUMN labels TEXT")?;
        }
        if !Self::has_column(conn, "tasks", "parent_card_id")? {
            conn.execute_batch("ALTER TABLE tasks ADD COLUMN parent_card_id TEXT")?;
        }
        if !Self::has_column(conn, "tasks", "source_url")? {
            conn.execute_batch("ALTER TABLE tasks ADD COLUMN source_url TEXT")?;
        }
        if !Self::has_column(conn, "tasks", "source_type")? {
            conn.execute_batch("ALTER TABLE tasks ADD COLUMN source_type TEXT")?;
        }
        if !Self::has_column(conn, "tasks", "sort_order")? {
            conn.execute_batch(
                "ALTER TABLE tasks ADD COLUMN sort_order INTEGER NOT NULL DEFAULT 0",
            )?;
        }
        if !Self::has_column(conn, "tasks", "assigned_at")? {
            conn.execute_batch("ALTER TABLE tasks ADD COLUMN assigned_at TEXT")?;
        }
        if !Self::has_column(conn, "tasks", "started_at")? {
            conn.execute_batch("ALTER TABLE tasks ADD COLUMN started_at TEXT")?;
        }
        if !Self::has_column(conn, "tasks", "completed_at")? {
            conn.execute_batch("ALTER TABLE tasks ADD COLUMN completed_at TEXT")?;
        }

        // Structured card fields parsed from issue body context.
        if !Self::has_column(conn, "tasks", "problem")? {
            conn.execute_batch("ALTER TABLE tasks ADD COLUMN problem TEXT")?;
            conn.execute_batch("ALTER TABLE tasks ADD COLUMN solution TEXT")?;
            conn.execute_batch("ALTER TABLE tasks ADD COLUMN acceptance TEXT")?;
            Self::backfill_parsed_fields(conn)?;
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

        // Migration 10: Bullpen archive -- nullable archived_at on reflections (#168).
        if !Self::has_column(conn, "reflections", "archived_at")? {
            conn.execute_batch("ALTER TABLE reflections ADD COLUMN archived_at TEXT;")?;
        }
        conn.execute_batch(
            "CREATE INDEX IF NOT EXISTS idx_reflections_archived \
             ON reflections(archived_at, created_at);",
        )?;

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

    /// Insert a new reflection for the given repository.
    ///
    /// Generates a UUIDv7 id and ISO 8601 timestamp automatically.
    /// The `audience` parameter controls visibility: "self" for private
    /// reflections, "team" for bullpen posts visible to all agents.
    #[allow(dead_code)]
    pub fn insert_reflection(&self, repo: &str, text: &str, audience: &str) -> Result<Reflection> {
        self.insert_reflection_with_meta(repo, text, audience, &ReflectionMeta::default())
    }

    /// Insert a new reflection with optional Synapse metadata.
    ///
    /// Like `insert_reflection` but accepts domain, tags, and parent_id
    /// for learning chain linking and classification.
    pub fn insert_reflection_with_meta(
        &self,
        repo: &str,
        text: &str,
        audience: &str,
        meta: &ReflectionMeta,
    ) -> Result<Reflection> {
        let id = Uuid::now_v7().to_string();
        let created_at = Utc::now().to_rfc3339();

        self.conn.execute(
            "INSERT INTO reflections (id, repo, text, created_at, audience, domain, tags, parent_id) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            rusqlite::params![
                &id, repo, text, &created_at, audience,
                &meta.domain, &meta.tags, &meta.parent_id,
            ],
        )?;

        Ok(Reflection {
            id,
            repo: repo.to_owned(),
            text: text.to_owned(),
            created_at,
            audience: audience.to_owned(),
            domain: meta.domain.clone(),
            tags: meta.tags.clone(),
            recall_count: 0,
            last_recalled_at: None,
            parent_id: meta.parent_id.clone(),
        })
    }

    /// Store an embedding BLOB for an existing reflection.
    pub fn store_embedding(&self, id: &str, embedding_bytes: &[u8]) -> Result<bool> {
        let rows = self.conn.execute(
            "UPDATE reflections SET embedding = ?1 WHERE id = ?2",
            rusqlite::params![embedding_bytes, id],
        )?;
        Ok(rows > 0)
    }

    /// Retrieve the embedding BLOB for a reflection, if it exists.
    pub fn get_embedding(&self, id: &str) -> Result<Option<Vec<u8>>> {
        let mut stmt = self
            .conn
            .prepare("SELECT embedding FROM reflections WHERE id = ?1")?;
        let mut rows = stmt.query_map([id], |row| {
            let blob: Option<Vec<u8>> = row.get(0)?;
            Ok(blob)
        })?;
        match rows.next() {
            Some(row) => Ok(row?),
            None => Ok(None),
        }
    }

    /// Retrieve all reflections that have embeddings, optionally filtered by repo.
    ///
    /// Returns (id, embedding_bytes) pairs for cosine similarity search.
    /// Pass `None` for cross-repo search (consult), or `Some(repo)` for
    /// repo-scoped search (recall).
    pub fn get_embeddings(&self, repo: Option<&str>) -> Result<Vec<(String, Vec<u8>)>> {
        let map_row = |row: &rusqlite::Row<'_>| -> rusqlite::Result<(String, Vec<u8>)> {
            Ok((row.get(0)?, row.get(1)?))
        };

        let base = "SELECT id, embedding FROM reflections WHERE embedding IS NOT NULL";
        let sql = match repo {
            Some(_) => format!("{base} AND repo = ?1"),
            None => base.to_owned(),
        };

        let mut stmt = self.conn.prepare(&sql)?;
        let rows = match repo {
            Some(r) => stmt.query_map([r], map_row)?,
            None => stmt.query_map([], map_row)?,
        };
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(LegionError::Database)
    }

    /// Get all reflection IDs that are missing embeddings.
    pub fn get_ids_without_embeddings(&self) -> Result<Vec<(String, String)>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, text FROM reflections WHERE embedding IS NULL ORDER BY created_at DESC",
        )?;
        let rows = stmt.query_map([], |row| {
            let id: String = row.get(0)?;
            let text: String = row.get(1)?;
            Ok((id, text))
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(LegionError::Database)
    }

    /// Retrieve the most recent reflections with embeddings for a repo.
    ///
    /// Returns (id, embedding_blob, text, created_at) tuples, ordered newest
    /// first, for near-duplicate detection on `legion reflect`. Only rows that
    /// have a non-NULL embedding are returned, so reflections that predate the
    /// embed backfill are naturally skipped.
    pub fn get_recent_reflections_with_embeddings(
        &self,
        repo: &str,
        limit: usize,
    ) -> Result<Vec<ReflectionWithEmbedding>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, embedding, text, created_at FROM reflections \
             WHERE repo = ?1 AND embedding IS NOT NULL \
             ORDER BY created_at DESC LIMIT ?2",
        )?;
        let rows = stmt.query_map(rusqlite::params![repo, limit], |row| {
            let id: String = row.get(0)?;
            let blob: Vec<u8> = row.get(1)?;
            let text: String = row.get(2)?;
            let created_at: String = row.get(3)?;
            Ok((id, blob, text, created_at))
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(LegionError::Database)
    }

    /// Increment a reflection's recall count and update last_recalled_at.
    ///
    /// Used by `legion boost` to mark a reflection as useful after being
    /// recalled and applied. Reflections with higher recall counts are
    /// ranked higher in future searches.
    pub fn boost_reflection(&self, id: &str) -> Result<bool> {
        let now = Utc::now().to_rfc3339();
        let rows = self.conn.execute(
            "UPDATE reflections SET recall_count = recall_count + 1, last_recalled_at = ?1 WHERE id = ?2",
            (&now, id),
        )?;
        Ok(rows > 0)
    }

    /// Retrieve a learning chain starting from the given reflection ID.
    ///
    /// Walks the parent_id links backward to find the chain root, then
    /// walks forward to collect all reflections in chronological order.
    /// Returns an empty vec if the ID does not exist.
    pub fn get_chain(&self, id: &str) -> Result<Vec<Reflection>> {
        // Walk backward to find the root
        let mut root_id = id.to_string();
        loop {
            let r = self.get_reflection_by_id(&root_id)?;
            match r {
                Some(ref reflection) => match &reflection.parent_id {
                    Some(pid) => root_id = pid.clone(),
                    None => break,
                },
                None => break,
            }
        }

        // Walk forward from root collecting children
        let mut chain = Vec::new();
        let mut current_id = Some(root_id);

        while let Some(cid) = current_id {
            match self.get_reflection_by_id(&cid)? {
                Some(r) => {
                    let next = self.find_child(&r.id)?;
                    chain.push(r);
                    current_id = next;
                }
                None => break,
            }
        }

        Ok(chain)
    }

    /// Find the child reflection that follows the given parent ID.
    fn find_child(&self, parent_id: &str) -> Result<Option<String>> {
        let mut stmt = self
            .conn
            .prepare("SELECT id FROM reflections WHERE parent_id = ?1 LIMIT 1")?;
        let mut rows = stmt.query_map([parent_id], |row| row.get::<_, String>(0))?;
        match rows.next() {
            Some(row) => Ok(Some(row?)),
            None => Ok(None),
        }
    }

    /// Retrieve a single reflection by its ID.
    ///
    /// Returns `None` if no reflection exists with the given ID.
    pub fn get_reflection_by_id(&self, id: &str) -> Result<Option<Reflection>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, repo, text, created_at, audience, domain, tags, recall_count, last_recalled_at, parent_id FROM reflections WHERE id = ?1",
        )?;

        let mut rows = stmt.query_map([id], map_reflection_row)?;

        match rows.next() {
            Some(row) => Ok(Some(row?)),
            None => Ok(None),
        }
    }

    /// Retrieve reflections by a list of IDs. Returns them in the order found
    /// (not necessarily the input order). Missing IDs are silently skipped.
    pub fn get_reflections_by_ids(&self, ids: &[&str]) -> Result<Vec<Reflection>> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let placeholders: Vec<&str> = ids.iter().map(|_| "?").collect();
        let sql = format!(
            "SELECT id, repo, text, created_at, audience, domain, tags, recall_count, \
             last_recalled_at, parent_id FROM reflections WHERE id IN ({})",
            placeholders.join(", ")
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let params: Vec<&dyn rusqlite::types::ToSql> = ids
            .iter()
            .map(|id| id as &dyn rusqlite::types::ToSql)
            .collect();
        let rows = stmt.query_map(params.as_slice(), map_reflection_row)?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(LegionError::Database)
    }

    /// Retrieve all reflections for a repository, ordered newest first.
    #[cfg(test)]
    pub fn get_reflections_by_repo(&self, repo: &str) -> Result<Vec<Reflection>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, repo, text, created_at, audience, domain, tags, recall_count, last_recalled_at, parent_id FROM reflections WHERE repo = ?1 ORDER BY created_at DESC",
        )?;

        let rows = stmt.query_map([repo], map_reflection_row)?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(LegionError::Database)
    }

    /// Retrieve the most recent reflections for a repository, limited by SQL.
    ///
    /// More efficient than `get_reflections_by_repo` when only a small
    /// number of results are needed, since the database handles the LIMIT.
    pub fn get_latest_self_reflections(&self, repo: &str, limit: usize) -> Result<Vec<Reflection>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, repo, text, created_at, audience, domain, tags, recall_count, last_recalled_at, parent_id FROM reflections WHERE repo = ?1 AND audience = 'self' ORDER BY created_at DESC LIMIT ?2",
        )?;

        let rows = stmt.query_map(rusqlite::params![repo, limit], map_reflection_row)?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(LegionError::Database)
    }

    /// Retrieve active (non-archived) bullpen posts, ordered newest first.
    pub fn get_board_posts(&self) -> Result<Vec<Reflection>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, repo, text, created_at, audience, domain, tags, recall_count, last_recalled_at, parent_id \
             FROM reflections WHERE audience = 'team' AND archived_at IS NULL ORDER BY created_at DESC",
        )?;

        let rows = stmt.query_map([], map_reflection_row)?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(LegionError::Database)
    }

    /// Retrieve up to `limit` active bullpen posts strictly after the given
    /// `(created_at, id)` cursor, ordered oldest first. Used by the MCP
    /// notifier thread to discover cross-process writes (from the CLI
    /// `legion post` command or from another MCP subprocess) that would
    /// otherwise be invisible to an in-process broadcast channel.
    ///
    /// The composite `(created_at, id)` cursor is the tiebreaker for two
    /// posts that share an identical `created_at` timestamp. Strict `>` on
    /// `created_at` alone is wrong when combined with `limit`: if the batch
    /// cap splits a tied-timestamp group, the next poll's cursor advances
    /// past the shared timestamp and subsequent rows at that timestamp are
    /// lost. Ordering and filtering by `(created_at, id)` eliminates this
    /// by giving every row a totally-ordered position (UUIDv7 ids embed a
    /// monotonic timestamp, so ties on `created_at` are almost always
    /// broken by `id` ordering anyway).
    ///
    /// `limit` caps the size of a single batch: if more rows exist beyond
    /// the cap, the cursor advances to the last row returned and the next
    /// poll catches the remainder.
    pub fn get_board_posts_since(
        &self,
        since_created_at: &str,
        since_id: &str,
        limit: usize,
    ) -> Result<Vec<Reflection>> {
        // strict `>` on the composite key: `(created_at > ?1) OR
        // (created_at = ?1 AND id > ?2)`. Flipping either comparator to
        // inclusive re-emits the cursor row on every poll tick and
        // produces an infinite duplicate-notification loop.
        let mut stmt = self.conn.prepare(
            "SELECT id, repo, text, created_at, audience, domain, tags, recall_count, last_recalled_at, parent_id \
             FROM reflections \
             WHERE audience = 'team' AND archived_at IS NULL \
               AND (created_at > ?1 OR (created_at = ?1 AND id > ?2)) \
             ORDER BY created_at ASC, id ASC \
             LIMIT ?3",
        )?;

        let rows = stmt.query_map(
            rusqlite::params![since_created_at, since_id, limit as i64],
            map_reflection_row,
        )?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(LegionError::Database)
    }

    /// Return the `(created_at, id)` of the most recent active team bullpen
    /// post, or `None` when the board is empty. Used by the MCP notifier
    /// thread as its startup cursor: seeding from the actual watermark of
    /// the rows the notifier is filtering against means a post committed in
    /// the same nanosecond as the notifier's seed is not dropped by a
    /// wall-clock race, and a future change that allows backdated inserts
    /// into non-team audience does not silently shift the notifier's
    /// starting point.
    pub fn get_board_cursor_watermark(&self) -> Result<Option<(String, String)>> {
        let mut stmt = self.conn.prepare(
            "SELECT created_at, id FROM reflections \
             WHERE audience = 'team' AND archived_at IS NULL \
             ORDER BY created_at DESC, id DESC \
             LIMIT 1",
        )?;
        let result = stmt
            .query_row([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .optional()
            .map_err(LegionError::Database)?;
        Ok(result)
    }

    /// Retrieve active bullpen posts unread by the given reader repo and
    /// atomically mark them as read. Posts created during the read are NOT
    /// marked, so they remain unread on the next call.
    ///
    /// Used by the channel backlog fetch so agents only see each post once.
    /// Race-safe: uses a single timestamp for both the SELECT filter upper
    /// bound and the mark_read UPDATE, inside a transaction.
    ///
    /// Fast path: when there are no unread posts, skips the INSERT entirely --
    /// every idle channel connect would otherwise pay a write-lock acquire on
    /// board_reads for no reason.
    pub fn get_and_mark_unread_board_posts(&self, reader_repo: &str) -> Result<Vec<Reflection>> {
        let now = Utc::now().to_rfc3339();

        let txn = self.conn.unchecked_transaction()?;

        let mut stmt = txn.prepare(
            "SELECT id, repo, text, created_at, audience, domain, tags, recall_count, last_recalled_at, parent_id \
             FROM reflections \
             WHERE audience = 'team' AND archived_at IS NULL \
             AND created_at > COALESCE( \
                 (SELECT last_read_at FROM board_reads WHERE reader_repo = ?1), \
                 '' \
             ) \
             AND created_at <= ?2 \
             ORDER BY created_at DESC",
        )?;

        let rows = stmt.query_map(rusqlite::params![reader_repo, &now], map_reflection_row)?;
        let posts: Vec<Reflection> = rows
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(LegionError::Database)?;

        drop(stmt);

        if posts.is_empty() {
            // Nothing to mark. Dropping the txn is a rollback of a pure read,
            // no write-lock pressure on board_reads.
            return Ok(posts);
        }

        // The WHERE guard on last_read_at is defensive against concurrent
        // writers or clock skew: if another process somehow wrote a later
        // timestamp between our SELECT and this UPDATE, we do not stomp it.
        // Under normal single-writer use this branch never fires, but it
        // preserves "last_read_at is monotonic non-decreasing" under any
        // future concurrent access.
        txn.execute(
            "INSERT INTO board_reads (reader_repo, last_read_at) VALUES (?1, ?2) \
             ON CONFLICT(reader_repo) DO UPDATE SET last_read_at = excluded.last_read_at \
             WHERE excluded.last_read_at > board_reads.last_read_at",
            rusqlite::params![reader_repo, &now],
        )?;

        txn.commit()?;

        Ok(posts)
    }

    /// Retrieve archived bullpen posts, ordered newest first.
    pub fn get_archived_posts(&self) -> Result<Vec<Reflection>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, repo, text, created_at, audience, domain, tags, recall_count, last_recalled_at, parent_id \
             FROM reflections WHERE audience = 'team' AND archived_at IS NOT NULL ORDER BY created_at DESC",
        )?;

        let rows = stmt.query_map([], map_reflection_row)?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(LegionError::Database)
    }

    /// Archive bullpen posts that all known readers have read.
    ///
    /// A post is archivable when every repo in board_reads has last_read_at
    /// after the post's created_at. Uses a single UPDATE with subquery to
    /// avoid race conditions between SELECT and UPDATE.
    /// Returns the number of posts archived.
    pub fn archive_read_posts(&self) -> Result<u64> {
        let now = Utc::now().to_rfc3339();

        let count = self.conn.execute(
            "UPDATE reflections SET archived_at = ?1 \
             WHERE audience = 'team' AND archived_at IS NULL \
             AND created_at < (SELECT MIN(last_read_at) FROM board_reads)",
            rusqlite::params![now],
        )?;

        Ok(count as u64)
    }

    /// Count team posts that are unread by the given reader repo.
    ///
    /// If the reader has no entry in board_reads, all team posts are unread.
    /// Only counts non-archived posts.
    pub fn get_unread_count(&self, reader_repo: &str) -> Result<u64> {
        let mut stmt = self.conn.prepare(
            "SELECT COUNT(*) FROM reflections WHERE audience = 'team' \
             AND archived_at IS NULL \
             AND created_at > COALESCE( \
                 (SELECT last_read_at FROM board_reads WHERE reader_repo = ?1), \
                 '' \
             )",
        )?;

        let count: u64 = stmt
            .query_row([reader_repo], |row| row.get(0))
            .map_err(LegionError::Database)?;

        Ok(count)
    }

    /// Mark all current bullpen posts as read for the given reader repo.
    ///
    /// Upserts the board_reads row with the current timestamp.
    pub fn mark_board_read(&self, reader_repo: &str) -> Result<()> {
        let now = Utc::now().to_rfc3339();

        self.conn.execute(
            "INSERT INTO board_reads (reader_repo, last_read_at) VALUES (?1, ?2) \
             ON CONFLICT(reader_repo) DO UPDATE SET last_read_at = excluded.last_read_at",
            (reader_repo, &now),
        )?;

        Ok(())
    }

    /// Find unhandled signals directed at a specific repo.
    ///
    /// Returns team posts that mention `@<repo_name>` (at start or mid-text)
    /// or `@all` that have not been handled by this specific repo yet.
    /// Uses the `watch_handled` table for per-repo tracking, so @all
    /// broadcasts wake each repo exactly once.
    pub fn get_unhandled_signals_for_repo(
        &self,
        repo_name: &str,
        since: Option<&str>,
    ) -> Result<Vec<Reflection>> {
        let pattern_start = format!("@{} %", repo_name);
        let pattern_mid = format!("%@{} %", repo_name);
        let pattern_all_start = "@all %";
        let pattern_all_mid = "%@all %";
        let since_clause = if since.is_some() {
            " AND r.created_at > ?6"
        } else {
            ""
        };
        let query = format!(
            "SELECT r.id, r.repo, r.text, r.created_at, r.audience, r.domain, r.tags, \
             r.recall_count, r.last_recalled_at, r.parent_id \
             FROM reflections r \
             LEFT JOIN watch_handled wh ON wh.signal_id = r.id AND wh.repo_name = ?5 \
             WHERE r.audience = 'team' \
               AND wh.signal_id IS NULL \
               AND (r.text LIKE ?1 OR r.text LIKE ?2 OR r.text LIKE ?3 OR r.text LIKE ?4) \
               AND r.repo != ?5{} \
             ORDER BY r.created_at ASC",
            since_clause
        );
        let mut stmt = self.conn.prepare(&query)?;
        let rows = if let Some(since_ts) = since {
            stmt.query_map(
                rusqlite::params![
                    &pattern_start,
                    &pattern_mid,
                    pattern_all_start,
                    pattern_all_mid,
                    repo_name,
                    since_ts
                ],
                map_reflection_row,
            )?
        } else {
            stmt.query_map(
                rusqlite::params![
                    &pattern_start,
                    &pattern_mid,
                    pattern_all_start,
                    pattern_all_mid,
                    repo_name
                ],
                map_reflection_row,
            )?
        };
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(LegionError::Database)
    }

    /// Delete stale watch_handled records older than a given timestamp.
    pub fn prune_watch_handled(&self, older_than: &str) -> Result<u64> {
        let rows = self.conn.execute(
            "DELETE FROM watch_handled WHERE handled_at < ?1",
            [older_than],
        )?;
        Ok(rows as u64)
    }

    /// Mark a signal as handled by a specific repo.
    ///
    /// Inserts into `watch_handled` so this signal will not be returned
    /// for this repo again. Works for both targeted and @all signals.
    pub fn mark_signal_handled_for_repo(&self, signal_id: &str, repo_name: &str) -> Result<bool> {
        let now = Utc::now().to_rfc3339();
        let rows = self.conn.execute(
            "INSERT OR IGNORE INTO watch_handled (signal_id, repo_name, handled_at) \
             VALUES (?1, ?2, ?3)",
            rusqlite::params![signal_id, repo_name, &now],
        )?;
        Ok(rows > 0)
    }

    /// Retrieve all reflections for reindexing.
    ///
    /// Returns every reflection in the database regardless of audience or
    /// repo. Used by the `reindex` command to rebuild the search index
    /// from the database (the source of truth).
    pub fn get_all_for_reindex(&self) -> Result<Vec<Reflection>> {
        let mut stmt = self
            .conn
            .prepare("SELECT id, repo, text, created_at, audience, domain, tags, recall_count, last_recalled_at, parent_id FROM reflections")?;
        let rows = stmt.query_map([], map_reflection_row)?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(LegionError::Database)
    }

    /// Get aggregate statistics, optionally filtered to a single repository.
    pub fn get_stats(&self, repo: Option<&str>) -> Result<Vec<RepoStats>> {
        let map_row = |row: &rusqlite::Row<'_>| -> rusqlite::Result<RepoStats> {
            Ok(RepoStats {
                repo: row.get(0)?,
                count: row.get(1)?,
                oldest: row.get(2)?,
                newest: row.get(3)?,
            })
        };

        let base = "SELECT repo, COUNT(*) as count, MIN(created_at) as oldest, \
                     MAX(created_at) as newest FROM reflections";

        let sql = match repo {
            Some(_) => format!("{base} WHERE repo = ?1 GROUP BY repo"),
            None => format!("{base} GROUP BY repo ORDER BY repo"),
        };

        let mut stmt = self.conn.prepare(&sql)?;

        let rows = match repo {
            Some(r) => stmt.query_map([r], map_row)?,
            None => stmt.query_map([], map_row)?,
        };

        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(LegionError::Database)
    }

    /// Get recent bullpen posts (within last N hours).
    pub fn get_recent_board_posts(&self, hours: i64) -> Result<Vec<Reflection>> {
        let cutoff = (Utc::now() - chrono::Duration::hours(hours)).to_rfc3339();
        let mut stmt = self.conn.prepare(
            "SELECT id, repo, text, created_at, audience, domain, tags, recall_count, last_recalled_at, parent_id \
             FROM reflections WHERE audience = 'team' AND archived_at IS NULL AND created_at > ?1 ORDER BY created_at DESC",
        )?;
        let rows = stmt.query_map([&cutoff], map_reflection_row)?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(LegionError::Database)
    }

    /// Get high-value reflections from other repos (by recall_count).
    ///
    /// Returns reflections with recall_count > 0 from repos other than
    /// the given one, ordered by recall_count descending.
    pub fn get_high_value_cross_repo(
        &self,
        exclude_repo: &str,
        limit: usize,
    ) -> Result<Vec<Reflection>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, repo, text, created_at, audience, domain, tags, recall_count, last_recalled_at, parent_id \
             FROM reflections WHERE repo != ?1 AND recall_count > 0 ORDER BY recall_count DESC LIMIT ?2",
        )?;
        let rows = stmt.query_map(rusqlite::params![exclude_repo, limit], map_reflection_row)?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(LegionError::Database)
    }

    /// Get all distinct repo names from reflections.
    pub fn get_distinct_repos(&self) -> Result<Vec<String>> {
        let mut stmt = self
            .conn
            .prepare("SELECT DISTINCT repo FROM reflections ORDER BY repo")?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(LegionError::Database)
    }

    /// Get unread bullpen counts for all known repos.
    ///
    /// Returns (repo_name, unread_count) pairs by calling get_unread_count
    /// for each distinct repo.
    pub fn get_unread_counts_all(&self) -> Result<Vec<(String, u64)>> {
        let repos = self.get_distinct_repos()?;
        let mut counts: Vec<(String, u64)> = Vec::with_capacity(repos.len());
        for repo in repos {
            let count = self.get_unread_count(&repo)?;
            counts.push((repo, count));
        }
        Ok(counts)
    }

    /// Get per-repo stats for the dashboard.
    ///
    /// Returns repo, reflection_count, boost_sum, team_post_count, and
    /// last_activity for each repo with reflections.
    pub fn get_dashboard_stats(&self) -> Result<Vec<DashboardRepoStats>> {
        let mut stmt = self.conn.prepare(
            "SELECT repo, COUNT(*) as cnt, \
             COALESCE(SUM(recall_count), 0) as boost, \
             SUM(CASE WHEN audience = 'team' THEN 1 ELSE 0 END) as team_cnt, \
             MAX(created_at) as last_act \
             FROM reflections GROUP BY repo ORDER BY repo",
        )?;

        let rows = stmt.query_map([], |row| {
            Ok(DashboardRepoStats {
                repo: row.get(0)?,
                reflection_count: row.get(1)?,
                boost_sum: row.get(2)?,
                team_post_count: row.get(3)?,
                last_activity: row.get(4)?,
            })
        })?;

        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(LegionError::Database)
    }

    /// Get all tasks regardless of repo (for kanban view).
    pub fn get_all_tasks(&self) -> Result<Vec<crate::task::Task>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, from_repo, to_repo, text, context, priority, status, note, created_at, updated_at \
             FROM tasks ORDER BY created_at DESC",
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
             FROM tasks WHERE id = ?1",
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
                 FROM tasks WHERE to_repo = ?1 ORDER BY created_at DESC"
            }
            crate::task::Direction::Outbound => {
                "SELECT id, from_repo, to_repo, text, context, priority, status, note, created_at, updated_at \
                 FROM tasks WHERE from_repo = ?1 ORDER BY created_at DESC"
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
            "UPDATE tasks SET status = ?1, note = COALESCE(?2, note), updated_at = ?3 WHERE id = ?4",
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
            .prepare("SELECT COUNT(*) FROM tasks WHERE to_repo = ?1 AND status = 'pending'")?;
        let count: u64 = stmt
            .query_row([repo], |row| row.get(0))
            .map_err(LegionError::Database)?;
        Ok(count)
    }

    /// Get pending tasks assigned to a repo (for surface output).
    pub fn get_pending_tasks_for_repo(&self, repo: &str) -> Result<Vec<crate::task::Task>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, from_repo, to_repo, text, context, priority, status, note, created_at, updated_at \
             FROM tasks WHERE to_repo = ?1 AND status = 'pending' ORDER BY created_at DESC",
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
             FROM tasks WHERE to_repo = ?1 AND status IN ('pending', 'accepted', 'blocked') \
             ORDER BY CASE priority WHEN 'high' THEN 0 WHEN 'med' THEN 1 WHEN 'low' THEN 2 END, created_at DESC",
        )?;
        let rows = stmt.query_map([repo], crate::task::map_task_row)?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(LegionError::Database)
    }

    /// Get the most recent created_at timestamp from reflections.
    pub fn get_max_created_at(&self) -> Result<Option<String>> {
        let mut stmt = self
            .conn
            .prepare("SELECT MAX(created_at) FROM reflections")?;
        let result: Option<String> = stmt
            .query_row([], |row| row.get(0))
            .map_err(LegionError::Database)?;
        Ok(result)
    }

    /// Get the most recent updated_at timestamp from tasks.
    pub fn get_max_task_updated_at(&self) -> Result<Option<String>> {
        let mut stmt = self.conn.prepare("SELECT MAX(updated_at) FROM tasks")?;
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
    ) -> Result<String> {
        let id = uuid::Uuid::now_v7().to_string();
        let now = chrono::Utc::now().to_rfc3339();
        let created_at = created_at_override.unwrap_or(&now);

        let parsed = context.map(crate::card_parse::parse_issue_body);
        let problem = parsed.as_ref().and_then(|p| p.problem.as_deref());
        let solution = parsed.as_ref().and_then(|p| p.solution.as_deref());
        let acceptance = parsed
            .as_ref()
            .map(|p| &p.acceptance)
            .filter(|a| !a.is_empty())
            .map(|a| a.join("\n"));

        self.conn.execute(
            "INSERT INTO tasks (id, from_repo, to_repo, text, context, priority, status, \
             labels, parent_card_id, source_url, source_type, created_at, updated_at, \
             problem, solution, acceptance) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'pending', ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
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
            ],
        )?;
        Ok(id)
    }

    /// Retrieve a single card by ID.
    pub fn get_card_by_id(&self, id: &str) -> Result<Option<crate::kanban::Card>> {
        let sql = format!("SELECT {} FROM tasks WHERE id = ?1", Self::CARD_COLUMNS);
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
    ) -> Result<Vec<crate::kanban::Card>> {
        let sql = match direction {
            crate::kanban::Direction::Inbound => {
                format!(
                    "SELECT {} FROM tasks WHERE to_repo = ?1 ORDER BY {}, sort_order ASC, created_at DESC",
                    Self::CARD_COLUMNS,
                    Self::PRIORITY_ORDER
                )
            }
            crate::kanban::Direction::Outbound => {
                format!(
                    "SELECT {} FROM tasks WHERE from_repo = ?1 ORDER BY created_at DESC",
                    Self::CARD_COLUMNS
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
            "SELECT {} FROM tasks ORDER BY {}, sort_order ASC, created_at DESC",
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
            .prepare("SELECT COUNT(*) FROM tasks WHERE to_repo = ?1 AND status = 'pending'")?;
        let count: u64 = stmt
            .query_row([repo], |row| row.get(0))
            .map_err(LegionError::Database)?;
        Ok(count)
    }

    /// Get pending cards assigned to a repo.
    pub fn get_pending_cards_for_repo(&self, repo: &str) -> Result<Vec<crate::kanban::Card>> {
        let sql = format!(
            "SELECT {} FROM tasks WHERE to_repo = ?1 AND status = 'pending' \
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
            "SELECT {} FROM tasks WHERE to_repo = ?1 AND status NOT IN ('done', 'cancelled') \
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
            "SELECT {} FROM tasks WHERE to_repo = ?1 AND status = 'pending' \
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
             WHERE id = ?3 AND status = 'pending'",
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
            "SELECT {} FROM tasks WHERE to_repo = ?1 AND status = 'pending' \
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
                 assigned_at = ?3, updated_at = ?4 WHERE id = ?5",
                rusqlite::params![status, note, now, now, id],
            )?,
            CardTimestamp::Started => self.conn.execute(
                "UPDATE tasks SET status = ?1, note = COALESCE(?2, note), \
                 started_at = ?3, updated_at = ?4 WHERE id = ?5",
                rusqlite::params![status, note, now, now, id],
            )?,
            CardTimestamp::Completed => self.conn.execute(
                "UPDATE tasks SET status = ?1, note = COALESCE(?2, note), \
                 completed_at = ?3, updated_at = ?4 WHERE id = ?5",
                rusqlite::params![status, note, now, now, id],
            )?,
            CardTimestamp::None => self.conn.execute(
                "UPDATE tasks SET status = ?1, note = COALESCE(?2, note), \
                 updated_at = ?3 WHERE id = ?4",
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
            "UPDATE tasks SET status = ?1, sort_order = ?2, updated_at = ?3{ts_sql} WHERE id = ?4"
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
             assigned_at = ?2, updated_at = ?3 WHERE id = ?4 AND status = 'backlog'",
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

    /// Get per-agent workload summary.
    pub fn get_agent_workloads(&self) -> Result<Vec<crate::kanban::AgentWorkload>> {
        let mut stmt = self.conn.prepare(
            "SELECT to_repo, \
             SUM(CASE WHEN status IN ('accepted', 'in-review', 'needs-input') THEN 1 ELSE 0 END) as active, \
             SUM(CASE WHEN status = 'pending' THEN 1 ELSE 0 END) as pending, \
             SUM(CASE WHEN status = 'blocked' THEN 1 ELSE 0 END) as blocked \
             FROM tasks WHERE status NOT IN ('done', 'cancelled') \
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
            "UPDATE tasks SET {} WHERE id = ?{}",
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

    // --- Schedule CRUD ---

    /// Insert a new schedule. Validates the cron expression and time window, computes next_run.
    pub fn insert_schedule(
        &self,
        name: &str,
        cron: &str,
        command: &str,
        repo: &str,
        active_start: Option<&str>,
        active_end: Option<&str>,
    ) -> Result<String> {
        // Validate time window if provided
        if let Some(s) = active_start {
            validate_hhmm(s)?;
        }
        if let Some(e) = active_end {
            validate_hhmm(e)?;
        }

        let now = Utc::now();
        let next_run = compute_next_run(cron, now)?;
        let id = Uuid::now_v7().to_string();
        let created_at = now.to_rfc3339();
        let next_run_str = next_run.to_rfc3339();

        self.conn.execute(
            "INSERT INTO schedules (id, name, cron, command, repo, enabled, next_run, created_at, active_start, active_end) \
             VALUES (?1, ?2, ?3, ?4, ?5, 1, ?6, ?7, ?8, ?9)",
            rusqlite::params![&id, name, cron, command, repo, &next_run_str, &created_at, active_start, active_end],
        )?;

        Ok(id)
    }

    /// Get all schedules that are enabled, due (next_run <= now), and within
    /// their active time window (if set).
    pub fn get_due_schedules(&self) -> Result<Vec<Schedule>> {
        let now = Utc::now();
        let now_str = now.to_rfc3339();
        let mut stmt = self.conn.prepare(
            "SELECT id, name, cron, command, repo, enabled, last_run, next_run, created_at, active_start, active_end \
             FROM schedules WHERE enabled = 1 AND next_run <= ?1",
        )?;
        let rows = stmt.query_map([&now_str], map_schedule_row)?;
        let all: Vec<Schedule> = rows
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(LegionError::Database)?;

        // Filter by active time window
        Ok(all
            .into_iter()
            .filter(|s| is_in_active_window(s, &now))
            .collect())
    }

    /// Mark a schedule as having just run. Updates last_run and computes next next_run.
    pub fn mark_schedule_run(&self, id: &str) -> Result<()> {
        // Fetch the cron expression to compute the next run
        let cron: String = self
            .conn
            .query_row("SELECT cron FROM schedules WHERE id = ?1", [id], |row| {
                row.get(0)
            })
            .map_err(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => {
                    LegionError::ScheduleNotFound(id.to_string())
                }
                other => LegionError::Database(other),
            })?;

        let now = Utc::now();
        let next_run = compute_next_run(&cron, now)?;
        let now_str = now.to_rfc3339();
        let next_run_str = next_run.to_rfc3339();

        self.conn.execute(
            "UPDATE schedules SET last_run = ?1, next_run = ?2 WHERE id = ?3",
            rusqlite::params![&now_str, &next_run_str, id],
        )?;

        Ok(())
    }

    /// List all schedules.
    pub fn list_schedules(&self) -> Result<Vec<Schedule>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, name, cron, command, repo, enabled, last_run, next_run, created_at, active_start, active_end \
             FROM schedules ORDER BY created_at",
        )?;
        let rows = stmt.query_map([], map_schedule_row)?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(LegionError::Database)
    }

    /// Toggle a schedule's enabled state. Returns false if schedule not found.
    pub fn toggle_schedule(&self, id: &str, enabled: bool) -> Result<bool> {
        let enabled_int: i32 = if enabled { 1 } else { 0 };
        let rows = self.conn.execute(
            "UPDATE schedules SET enabled = ?1 WHERE id = ?2",
            rusqlite::params![enabled_int, id],
        )?;
        Ok(rows > 0)
    }

    /// Delete a schedule by ID. Returns false if schedule not found.
    pub fn delete_schedule(&self, id: &str) -> Result<bool> {
        let rows = self
            .conn
            .execute("DELETE FROM schedules WHERE id = ?1", [id])?;
        Ok(rows > 0)
    }

    /// Update a schedule's cron expression and/or active window.
    pub fn update_schedule(
        &self,
        id: &str,
        cron: Option<&str>,
        active_start: Option<&str>,
        active_end: Option<&str>,
    ) -> Result<bool> {
        let mut updates: Vec<String> = Vec::new();
        let mut params: Vec<String> = Vec::new();

        if let Some(c) = cron {
            updates.push(format!("cron = ?{}", params.len() + 1));
            params.push(c.to_string());
        }
        if let Some(s) = active_start {
            updates.push(format!("active_start = ?{}", params.len() + 1));
            params.push(s.to_string());
        }
        if let Some(e) = active_end {
            updates.push(format!("active_end = ?{}", params.len() + 1));
            params.push(e.to_string());
        }

        if updates.is_empty() {
            return Ok(false);
        }

        let query = format!(
            "UPDATE schedules SET {} WHERE id = ?{}",
            updates.join(", "),
            params.len() + 1
        );
        params.push(id.to_string());

        let param_refs: Vec<&dyn rusqlite::types::ToSql> = params
            .iter()
            .map(|s| s as &dyn rusqlite::types::ToSql)
            .collect();
        let rows = self.conn.execute(&query, param_refs.as_slice())?;
        Ok(rows > 0)
    }

    /// Get recently extended learning chains.
    ///
    /// Returns reflections that have a parent_id and were created within
    /// the last N hours, indicating a chain was recently extended.
    pub fn get_recent_chain_extensions(&self, hours: i64) -> Result<Vec<Reflection>> {
        let cutoff = (Utc::now() - chrono::Duration::hours(hours)).to_rfc3339();
        let mut stmt = self.conn.prepare(
            "SELECT id, repo, text, created_at, audience, domain, tags, recall_count, last_recalled_at, parent_id \
             FROM reflections WHERE parent_id IS NOT NULL AND created_at > ?1 ORDER BY created_at DESC",
        )?;
        let rows = stmt.query_map([&cutoff], map_reflection_row)?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(LegionError::Database)
    }

    // -- Health Samples ----------------------------------------------------------

    /// Insert a health sample into the database.
    pub fn insert_health_sample(&self, sample: &crate::health::HealthSample) -> Result<String> {
        self.conn.execute(
            "INSERT INTO health_samples (id, hostname, sampled_at, cpu_usage_pct, \
             load_avg_1, load_avg_5, load_avg_15, cpu_core_count, mem_total_bytes, \
             mem_used_bytes, mem_usage_pct, swap_total_bytes, swap_used_bytes, \
             cpu_temp_celsius, agents_active, pressure) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)",
            rusqlite::params![
                sample.id,
                sample.hostname,
                sample.sampled_at,
                sample.cpu_usage_pct,
                sample.load_avg_1,
                sample.load_avg_5,
                sample.load_avg_15,
                sample.cpu_core_count,
                sample.mem_total_bytes,
                sample.mem_used_bytes,
                sample.mem_usage_pct,
                sample.swap_total_bytes,
                sample.swap_used_bytes,
                sample.cpu_temp_celsius,
                sample.agents_active,
                sample.pressure,
            ],
        )?;
        Ok(sample.id.clone())
    }

    /// Get the most recent health sample for a hostname.
    #[allow(dead_code)]
    pub fn get_latest_health(&self, hostname: &str) -> Result<Option<crate::health::HealthSample>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, hostname, sampled_at, cpu_usage_pct, load_avg_1, load_avg_5, \
             load_avg_15, cpu_core_count, mem_total_bytes, mem_used_bytes, mem_usage_pct, \
             swap_total_bytes, swap_used_bytes, cpu_temp_celsius, agents_active, pressure \
             FROM health_samples WHERE hostname = ?1 ORDER BY sampled_at DESC LIMIT 1",
        )?;
        let mut rows = stmt.query_map([hostname], map_health_row)?;
        match rows.next() {
            Some(row) => Ok(Some(row.map_err(LegionError::Database)?)),
            None => Ok(None),
        }
    }

    /// Get health samples for a hostname since a given timestamp.
    pub fn get_health_history(
        &self,
        hostname: &str,
        since: &str,
    ) -> Result<Vec<crate::health::HealthSample>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, hostname, sampled_at, cpu_usage_pct, load_avg_1, load_avg_5, \
             load_avg_15, cpu_core_count, mem_total_bytes, mem_used_bytes, mem_usage_pct, \
             swap_total_bytes, swap_used_bytes, cpu_temp_celsius, agents_active, pressure \
             FROM health_samples WHERE hostname = ?1 AND sampled_at > ?2 \
             ORDER BY sampled_at DESC",
        )?;
        let rows = stmt.query_map(rusqlite::params![hostname, since], map_health_row)?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(LegionError::Database)
    }

    /// Get health samples from all hostnames since a given timestamp.
    pub fn get_health_all_hosts(&self, since: &str) -> Result<Vec<crate::health::HealthSample>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, hostname, sampled_at, cpu_usage_pct, load_avg_1, load_avg_5, \
             load_avg_15, cpu_core_count, mem_total_bytes, mem_used_bytes, mem_usage_pct, \
             swap_total_bytes, swap_used_bytes, cpu_temp_celsius, agents_active, pressure \
             FROM health_samples WHERE sampled_at > ?1 ORDER BY sampled_at DESC",
        )?;
        let rows = stmt.query_map([since], map_health_row)?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(LegionError::Database)
    }

    /// Delete health samples older than a given timestamp.
    pub fn prune_health_samples(&self, older_than: &str) -> Result<u64> {
        let rows = self.conn.execute(
            "DELETE FROM health_samples WHERE sampled_at < ?1",
            [older_than],
        )?;
        Ok(rows as u64)
    }

    /// Rename a repo across all tables. Returns total rows updated.
    pub fn rename_repo(&self, from: &str, to: &str) -> Result<RenameCounts> {
        // unchecked_transaction because Database uses &self (shared ref),
        // but rusqlite::Connection::transaction() requires &mut self.
        // Safe here: no concurrent access within this function.
        let tx = self.conn.unchecked_transaction()?;

        let reflections = tx.execute(
            "UPDATE reflections SET repo = ?1 WHERE repo = ?2",
            [to, from],
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

        let schedules =
            tx.execute("UPDATE schedules SET repo = ?1 WHERE repo = ?2", [to, from])? as u64;

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

/// Map a database row to a HealthSample struct.
fn map_health_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<crate::health::HealthSample> {
    Ok(crate::health::HealthSample {
        id: row.get(0)?,
        hostname: row.get(1)?,
        sampled_at: row.get(2)?,
        cpu_usage_pct: row.get(3)?,
        load_avg_1: row.get(4)?,
        load_avg_5: row.get(5)?,
        load_avg_15: row.get(6)?,
        cpu_core_count: row.get(7)?,
        mem_total_bytes: row.get(8)?,
        mem_used_bytes: row.get(9)?,
        mem_usage_pct: row.get(10)?,
        swap_total_bytes: row.get(11)?,
        swap_used_bytes: row.get(12)?,
        cpu_temp_celsius: row.get(13)?,
        agents_active: row.get(14)?,
        pressure: row.get(15)?,
    })
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
    pub result: String,
    pub findings_count: u64,
    pub details: Option<String>,
    pub created_at: String,
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
        result: &str,
        findings_count: u64,
        details: Option<&str>,
    ) -> Result<QualityGateRow> {
        let id = Uuid::now_v7().to_string();
        let created_at = Utc::now().to_rfc3339();
        self.conn.execute(
            "INSERT INTO quality_gates \
             (id, branch, commit_hash, skill, result, findings_count, details, created_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            rusqlite::params![
                &id,
                branch,
                commit_hash,
                skill,
                result,
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
            result: result.to_owned(),
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
            let findings_count_i64: i64 = row.get(5)?;
            Ok(QualityGateRow {
                id: row.get(0)?,
                branch: row.get(1)?,
                commit_hash: row.get(2)?,
                skill: row.get(3)?,
                result: row.get(4)?,
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

    /// Create an in-memory database for testing.
    fn test_db() -> Database {
        let dir = tempfile::tempdir().unwrap();
        Database::open(&dir.path().join("test.db")).unwrap()
    }

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
    fn insert_and_retrieve_reflection() {
        let db = test_db();
        let r = db
            .insert_reflection("kelex", "mapping rules are fragile", "self")
            .unwrap();
        assert_eq!(r.repo, "kelex");
        assert_eq!(r.text, "mapping rules are fragile");
        assert!(!r.id.is_empty());

        let all = db.get_reflections_by_repo("kelex").unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].id, r.id);
    }

    #[test]
    fn reflections_scoped_to_repo() {
        let db = test_db();
        db.insert_reflection("kelex", "reflection 1", "self")
            .unwrap();
        db.insert_reflection("rafters", "reflection 2", "self")
            .unwrap();

        let kelex = db.get_reflections_by_repo("kelex").unwrap();
        assert_eq!(kelex.len(), 1);
        assert_eq!(kelex[0].text, "reflection 1");
    }

    #[test]
    fn stats_returns_counts() {
        let db = test_db();
        db.insert_reflection("kelex", "one", "self").unwrap();
        db.insert_reflection("kelex", "two", "self").unwrap();
        db.insert_reflection("rafters", "three", "self").unwrap();

        let stats = db.get_stats(None).unwrap();
        assert_eq!(stats.len(), 2);

        let kelex_stats = db.get_stats(Some("kelex")).unwrap();
        assert_eq!(kelex_stats.len(), 1);
        assert_eq!(kelex_stats[0].count, 2);
    }

    #[test]
    fn ids_are_uuidv7() {
        let db = test_db();
        let r = db.insert_reflection("test", "text", "self").unwrap();
        assert_eq!(r.id.len(), 36);
        // UUIDv7 has version nibble '7' at position 14
        assert_eq!(&r.id[14..15], "7");
    }

    #[test]
    fn created_at_is_iso8601() {
        let db = test_db();
        let r = db.insert_reflection("test", "text", "self").unwrap();
        // ISO 8601 strings contain 'T' separator and '+' or end with 'Z'
        assert!(r.created_at.contains('T'));
    }

    #[test]
    fn empty_repo_returns_empty_vec() {
        let db = test_db();
        let results = db.get_reflections_by_repo("nonexistent").unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn stats_empty_database() {
        let db = test_db();
        let stats = db.get_stats(None).unwrap();
        assert!(stats.is_empty());
    }

    #[test]
    fn stats_for_nonexistent_repo() {
        let db = test_db();
        db.insert_reflection("kelex", "one", "self").unwrap();
        let stats = db.get_stats(Some("nonexistent")).unwrap();
        assert!(stats.is_empty());
    }

    #[test]
    fn insert_reflection_with_audience_self() {
        let db = test_db();
        let r = db.insert_reflection("kelex", "test", "self").unwrap();
        assert_eq!(r.audience, "self");
    }

    #[test]
    fn insert_reflection_with_audience_team() {
        let db = test_db();
        let r = db
            .insert_reflection("rafters", "night shift musings", "team")
            .unwrap();
        assert_eq!(r.audience, "team");
    }

    #[test]
    fn get_board_posts_returns_only_team() {
        let db = test_db();
        db.insert_reflection("kelex", "private note", "self")
            .unwrap();
        db.insert_reflection("rafters", "shared insight", "team")
            .unwrap();
        let posts = db.get_board_posts().unwrap();
        assert_eq!(posts.len(), 1);
        assert_eq!(posts[0].audience, "team");
    }

    #[test]
    fn get_board_posts_since_excludes_cursor_row_and_self_posts() {
        let db = test_db();

        // Insert three rows with increasing created_at: older, middle, newer.
        // Use insert_reflection so created_at is a real ISO 8601 stamp.
        let older = db.insert_reflection("kelex", "old", "team").unwrap();
        // Private reflection between team posts -- must NOT appear.
        db.insert_reflection("kelex", "not shared", "self").unwrap();
        let newer = db.insert_reflection("rafters", "new", "team").unwrap();

        // Cursor at `(older.created_at, older.id)` must return only `newer`
        // (strict `>` on the composite key).
        let batch = db
            .get_board_posts_since(&older.created_at, &older.id, 100)
            .expect("query");
        assert_eq!(batch.len(), 1);
        assert_eq!(batch[0].id, newer.id);
        assert_eq!(batch[0].audience, "team");
    }

    #[test]
    fn get_board_posts_since_breaks_ties_on_id_component() {
        // Two posts with an identical `created_at` must still be visited in
        // deterministic order by id, and splitting a tied group with a tight
        // LIMIT must not lose rows: the cursor advances to `(created_at, id)`
        // of the last row returned, and the next query finds the tied-but-
        // higher-id row via the `created_at = ? AND id > ?` branch.
        let db = test_db();
        let shared_ts = "2026-04-11T12:00:00.000000000+00:00";

        // Insert two rows with IDENTICAL created_at (bypassing insert_reflection
        // so we can force the timestamp collision). UUIDv7 ids naturally sort
        // in the same order they are generated, so order_a < order_b.
        let id_a = "01000000-0000-7000-8000-000000000001";
        let id_b = "01000000-0000-7000-8000-000000000002";

        db.conn
            .execute(
                "INSERT INTO reflections (id, repo, text, created_at, audience) \
                 VALUES (?1, 'kelex', 'tied A', ?2, 'team')",
                rusqlite::params![id_a, shared_ts],
            )
            .unwrap();
        db.conn
            .execute(
                "INSERT INTO reflections (id, repo, text, created_at, audience) \
                 VALUES (?1, 'kelex', 'tied B', ?2, 'team')",
                rusqlite::params![id_b, shared_ts],
            )
            .unwrap();

        // First batch with limit=1 must return id_a and advance the cursor
        // to `(shared_ts, id_a)`.
        let batch = db
            .get_board_posts_since("2026-04-11T00:00:00+00:00", "", 1)
            .expect("query");
        assert_eq!(batch.len(), 1);
        assert_eq!(batch[0].id, id_a);

        // Second batch using the advanced cursor must find id_b via the
        // tiebreaker branch -- this is the regression guard against the
        // "tied timestamp + strict > on created_at alone = silent row drop"
        // bug surfaced in PR #222 review.
        let batch2 = db
            .get_board_posts_since(shared_ts, id_a, 10)
            .expect("query");
        assert_eq!(batch2.len(), 1);
        assert_eq!(batch2[0].id, id_b);
    }

    #[test]
    fn get_board_posts_since_honors_limit_and_ordering() {
        let db = test_db();

        // Seed a sentinel row so we have a stable pre-cursor.
        let sentinel = db.insert_reflection("seed", "sentinel", "team").unwrap();

        // Insert five team posts after the sentinel. insert_reflection writes
        // `created_at = Utc::now().to_rfc3339()` with nanosecond precision
        // via chrono; if the OS clock resolution ever collapses sequential
        // inserts into identical stamps, the composite cursor (covered by
        // `get_board_posts_since_breaks_ties_on_id_component`) still keeps
        // this test deterministic via UUIDv7 ordering.
        let mut expected_ids = Vec::new();
        for i in 0..5 {
            let r = db
                .insert_reflection("kelex", &format!("msg {i}"), "team")
                .unwrap();
            expected_ids.push(r.id);
        }

        // Request at most 3 rows after the sentinel -- must return the first
        // three in insertion order.
        let batch = db
            .get_board_posts_since(&sentinel.created_at, &sentinel.id, 3)
            .expect("query");
        assert_eq!(batch.len(), 3);
        assert_eq!(batch[0].id, expected_ids[0]);
        assert_eq!(batch[1].id, expected_ids[1]);
        assert_eq!(batch[2].id, expected_ids[2]);

        // Advance the cursor to the last row returned; the next call returns
        // the remaining two.
        let next = db
            .get_board_posts_since(&batch[2].created_at, &batch[2].id, 3)
            .expect("query");
        assert_eq!(next.len(), 2);
        assert_eq!(next[0].id, expected_ids[3]);
        assert_eq!(next[1].id, expected_ids[4]);

        // Cursor at the newest row returns an empty batch (idle poll path).
        let idle = db
            .get_board_posts_since(&next[1].created_at, &next[1].id, 10)
            .expect("query");
        assert!(idle.is_empty());
    }

    #[test]
    fn get_board_posts_since_excludes_archived() {
        // The query must filter out archived_at rows so a bullpen archive
        // pass does not re-notify every archived post on next startup.
        let db = test_db();
        let live = db.insert_reflection("kelex", "live", "team").unwrap();
        let _archived = db
            .insert_reflection("kelex", "will archive", "team")
            .unwrap();

        // Archive the second row directly (no public archive helper for a
        // single id -- the test exercises the invariant the SQL relies on).
        db.conn
            .execute(
                "UPDATE reflections SET archived_at = '2026-04-11T00:00:00+00:00' WHERE text = 'will archive'",
                [],
            )
            .unwrap();

        // Cursor from the very beginning should still return only the live
        // row.
        let batch = db
            .get_board_posts_since("2026-01-01T00:00:00+00:00", "", 100)
            .expect("query");
        assert_eq!(batch.len(), 1);
        assert_eq!(batch[0].id, live.id);
    }

    #[test]
    fn get_board_cursor_watermark_empty_and_populated() {
        let db = test_db();

        // Empty table -> None.
        assert!(db.get_board_cursor_watermark().unwrap().is_none());

        // Single self reflection -> None (watermark is team-only).
        db.insert_reflection("kelex", "private", "self").unwrap();
        assert!(db.get_board_cursor_watermark().unwrap().is_none());

        // Add a team post -> watermark is that row.
        let team = db.insert_reflection("kelex", "shared", "team").unwrap();
        let watermark = db.get_board_cursor_watermark().unwrap();
        assert_eq!(watermark, Some((team.created_at.clone(), team.id.clone())));

        // Add a newer team post -> watermark advances.
        let newer = db.insert_reflection("rafters", "shared 2", "team").unwrap();
        let watermark = db.get_board_cursor_watermark().unwrap();
        assert_eq!(watermark, Some((newer.created_at, newer.id)));
    }

    #[test]
    fn unread_count_all_unread_when_no_reads() {
        let db = test_db();
        db.insert_reflection("rafters", "post 1", "team").unwrap();
        db.insert_reflection("kelex", "post 2", "team").unwrap();
        assert_eq!(db.get_unread_count("legion").unwrap(), 2);
    }

    #[test]
    fn mark_board_read_resets_unread_count() {
        let db = test_db();
        db.insert_reflection("rafters", "old post", "team").unwrap();
        db.mark_board_read("kelex").unwrap();
        assert_eq!(db.get_unread_count("kelex").unwrap(), 0);
    }

    #[test]
    fn get_and_mark_unread_delivers_once() {
        let db = test_db();
        db.insert_reflection("rafters", "post 1", "team").unwrap();
        db.insert_reflection("kelex", "post 2", "team").unwrap();

        // First call delivers both posts.
        let first = db.get_and_mark_unread_board_posts("legion").unwrap();
        assert_eq!(first.len(), 2);

        // Second call delivers nothing -- they were marked read.
        let second = db.get_and_mark_unread_board_posts("legion").unwrap();
        assert!(
            second.is_empty(),
            "expected empty on second call, got {}",
            second.len()
        );
    }

    #[test]
    fn get_and_mark_unread_delivers_new_posts_after_mark() {
        let db = test_db();
        db.insert_reflection("rafters", "first", "team").unwrap();

        // Read the first post -- marks as read.
        let first = db.get_and_mark_unread_board_posts("legion").unwrap();
        assert_eq!(first.len(), 1);

        // New post arrives after the read.
        db.insert_reflection("kelex", "second", "team").unwrap();

        // Second call delivers only the new post.
        let second = db.get_and_mark_unread_board_posts("legion").unwrap();
        assert_eq!(second.len(), 1);
        assert_eq!(second[0].text, "second");
    }

    #[test]
    fn get_and_mark_unread_is_per_reader() {
        let db = test_db();
        db.insert_reflection("rafters", "post", "team").unwrap();

        // legion reads -- marked for legion only.
        let legion_posts = db.get_and_mark_unread_board_posts("legion").unwrap();
        assert_eq!(legion_posts.len(), 1);

        // kelex still sees it as unread.
        let kelex_posts = db.get_and_mark_unread_board_posts("kelex").unwrap();
        assert_eq!(kelex_posts.len(), 1);
    }

    #[test]
    fn get_all_for_reindex_returns_all_reflections() {
        let db = test_db();
        db.insert_reflection("kelex", "one", "self").unwrap();
        db.insert_reflection("rafters", "two", "team").unwrap();
        db.insert_reflection("platform", "three", "self").unwrap();

        let all = db.get_all_for_reindex().unwrap();
        assert_eq!(all.len(), 3);

        let repos: Vec<&str> = all.iter().map(|r| r.repo.as_str()).collect();
        assert!(repos.contains(&"kelex"));
        assert!(repos.contains(&"rafters"));
        assert!(repos.contains(&"platform"));
    }

    #[test]
    fn get_all_for_reindex_empty_db() {
        let db = test_db();
        let all = db.get_all_for_reindex().unwrap();
        assert!(all.is_empty());
    }

    #[test]
    fn existing_reflections_default_to_self() {
        let db = test_db();
        let r = db
            .insert_reflection("test", "old reflection", "self")
            .unwrap();
        assert_eq!(r.audience, "self");
        let posts = db.get_board_posts().unwrap();
        assert!(posts.is_empty());
    }

    #[test]
    fn get_board_posts_ordered_newest_first() {
        let db = test_db();
        db.insert_reflection("kelex", "first post", "team").unwrap();
        db.insert_reflection("rafters", "second post", "team")
            .unwrap();
        let posts = db.get_board_posts().unwrap();
        assert_eq!(posts.len(), 2);
        // Newest first means second post should be first in results
        assert_eq!(posts[0].text, "second post");
        assert_eq!(posts[1].text, "first post");
    }

    #[test]
    fn mark_board_read_is_idempotent() {
        let db = test_db();
        db.insert_reflection("rafters", "a post", "team").unwrap();
        db.mark_board_read("kelex").unwrap();
        db.mark_board_read("kelex").unwrap();
        assert_eq!(db.get_unread_count("kelex").unwrap(), 0);
    }

    #[test]
    fn unread_count_tracks_new_posts_after_read() {
        let db = test_db();
        db.insert_reflection("rafters", "old post", "team").unwrap();
        db.mark_board_read("kelex").unwrap();
        assert_eq!(db.get_unread_count("kelex").unwrap(), 0);

        // New post after marking read should be unread
        // Small sleep to ensure timestamp differs
        std::thread::sleep(std::time::Duration::from_millis(10));
        db.insert_reflection("platform", "new post", "team")
            .unwrap();
        assert_eq!(db.get_unread_count("kelex").unwrap(), 1);
    }

    #[test]
    fn insert_with_meta_stores_domain_and_tags() {
        let db = test_db();
        let meta = ReflectionMeta {
            domain: Some("color-tokens".into()),
            tags: Some("semantic-tokens,consumer".into()),
            parent_id: None,
        };
        let r = db
            .insert_reflection_with_meta("kelex", "oklch insight", "self", &meta)
            .unwrap();
        assert_eq!(r.domain.as_deref(), Some("color-tokens"));
        assert_eq!(r.tags.as_deref(), Some("semantic-tokens,consumer"));
        assert!(r.parent_id.is_none());

        let fetched = db.get_reflection_by_id(&r.id).unwrap().unwrap();
        assert_eq!(fetched.domain.as_deref(), Some("color-tokens"));
        assert_eq!(fetched.tags.as_deref(), Some("semantic-tokens,consumer"));
    }

    #[test]
    fn insert_with_meta_stores_parent_id() {
        let db = test_db();
        let parent = db.insert_reflection("kelex", "first", "self").unwrap();
        let meta = ReflectionMeta {
            domain: None,
            tags: None,
            parent_id: Some(parent.id.clone()),
        };
        let child = db
            .insert_reflection_with_meta("kelex", "follows up", "self", &meta)
            .unwrap();
        assert_eq!(child.parent_id.as_deref(), Some(parent.id.as_str()));
    }

    #[test]
    fn boost_increments_recall_count() {
        let db = test_db();
        let r = db
            .insert_reflection("kelex", "useful insight", "self")
            .unwrap();
        assert_eq!(r.recall_count, 0);
        assert!(r.last_recalled_at.is_none());

        let found = db.boost_reflection(&r.id).unwrap();
        assert!(found);

        let boosted = db.get_reflection_by_id(&r.id).unwrap().unwrap();
        assert_eq!(boosted.recall_count, 1);
        assert!(boosted.last_recalled_at.is_some());

        db.boost_reflection(&r.id).unwrap();
        let double = db.get_reflection_by_id(&r.id).unwrap().unwrap();
        assert_eq!(double.recall_count, 2);
    }

    #[test]
    fn boost_nonexistent_returns_false() {
        let db = test_db();
        let found = db.boost_reflection("nonexistent-id").unwrap();
        assert!(!found);
    }

    #[test]
    fn get_chain_single_node() {
        let db = test_db();
        let r = db.insert_reflection("kelex", "standalone", "self").unwrap();
        let chain = db.get_chain(&r.id).unwrap();
        assert_eq!(chain.len(), 1);
        assert_eq!(chain[0].id, r.id);
    }

    #[test]
    fn get_chain_three_links() {
        let db = test_db();
        let first = db
            .insert_reflection("kelex", "root insight", "self")
            .unwrap();
        let second = db
            .insert_reflection_with_meta(
                "kelex",
                "builds on root",
                "self",
                &ReflectionMeta {
                    parent_id: Some(first.id.clone()),
                    ..Default::default()
                },
            )
            .unwrap();
        let third = db
            .insert_reflection_with_meta(
                "kelex",
                "final refinement",
                "self",
                &ReflectionMeta {
                    parent_id: Some(second.id.clone()),
                    ..Default::default()
                },
            )
            .unwrap();

        // Querying from any node should return the full chain in order
        let chain = db.get_chain(&third.id).unwrap();
        assert_eq!(chain.len(), 3);
        assert_eq!(chain[0].id, first.id);
        assert_eq!(chain[1].id, second.id);
        assert_eq!(chain[2].id, third.id);

        let from_middle = db.get_chain(&second.id).unwrap();
        assert_eq!(from_middle.len(), 3);
        assert_eq!(from_middle[0].id, first.id);
    }

    #[test]
    fn get_chain_nonexistent_returns_empty() {
        let db = test_db();
        let chain = db.get_chain("nonexistent").unwrap();
        assert!(chain.is_empty());
    }

    #[test]
    fn get_reflection_by_id_found() {
        let db = test_db();
        let r = db.insert_reflection("kelex", "findable", "self").unwrap();
        let found = db.get_reflection_by_id(&r.id).unwrap();
        assert!(found.is_some());
        assert_eq!(found.unwrap().text, "findable");
    }

    #[test]
    fn get_reflection_by_id_not_found() {
        let db = test_db();
        let found = db.get_reflection_by_id("no-such-id").unwrap();
        assert!(found.is_none());
    }

    #[test]
    fn parse_hhmm_valid() {
        assert_eq!(parse_hhmm("00:00"), Some(0));
        assert_eq!(parse_hhmm("23:59"), Some(23 * 60 + 59));
        assert_eq!(parse_hhmm("07:30"), Some(7 * 60 + 30));
    }

    #[test]
    fn parse_hhmm_invalid() {
        assert_eq!(parse_hhmm("24:00"), None);
        assert_eq!(parse_hhmm("12:60"), None);
        assert_eq!(parse_hhmm("garbage"), None);
        assert_eq!(parse_hhmm(""), None);
        assert_eq!(parse_hhmm("12"), None);
    }

    #[test]
    fn active_window_no_window_always_active() {
        let schedule = Schedule {
            id: String::new(),
            name: String::new(),
            cron: String::new(),
            command: String::new(),
            repo: String::new(),
            enabled: true,
            last_run: None,
            next_run: String::new(),
            created_at: String::new(),
            active_start: None,
            active_end: None,
        };
        let now = Utc::now();
        assert!(is_in_active_window(&schedule, &now));
    }

    #[test]
    fn active_window_same_day() {
        let mut schedule = Schedule {
            id: String::new(),
            name: String::new(),
            cron: String::new(),
            command: String::new(),
            repo: String::new(),
            enabled: true,
            last_run: None,
            next_run: String::new(),
            created_at: String::new(),
            active_start: Some("09:00".to_string()),
            active_end: Some("17:00".to_string()),
        };

        // 12:00 is within 09:00-17:00
        let noon = chrono::NaiveDate::from_ymd_opt(2026, 3, 22)
            .unwrap()
            .and_hms_opt(12, 0, 0)
            .unwrap()
            .and_utc();
        assert!(is_in_active_window(&schedule, &noon));

        // 08:00 is outside 09:00-17:00
        let early = chrono::NaiveDate::from_ymd_opt(2026, 3, 22)
            .unwrap()
            .and_hms_opt(8, 0, 0)
            .unwrap()
            .and_utc();
        assert!(!is_in_active_window(&schedule, &early));

        // 17:00 is at the boundary (exclusive end)
        let boundary = chrono::NaiveDate::from_ymd_opt(2026, 3, 22)
            .unwrap()
            .and_hms_opt(17, 0, 0)
            .unwrap()
            .and_utc();
        assert!(!is_in_active_window(&schedule, &boundary));

        // Unparseable window falls back to always active
        schedule.active_start = Some("garbage".to_string());
        assert!(is_in_active_window(&schedule, &noon));
    }

    #[test]
    fn active_window_overnight() {
        let schedule = Schedule {
            id: String::new(),
            name: String::new(),
            cron: String::new(),
            command: String::new(),
            repo: String::new(),
            enabled: true,
            last_run: None,
            next_run: String::new(),
            created_at: String::new(),
            active_start: Some("23:00".to_string()),
            active_end: Some("07:00".to_string()),
        };

        // 01:00 is within 23:00-07:00 (after midnight)
        let late_night = chrono::NaiveDate::from_ymd_opt(2026, 3, 22)
            .unwrap()
            .and_hms_opt(1, 0, 0)
            .unwrap()
            .and_utc();
        assert!(is_in_active_window(&schedule, &late_night));

        // 23:30 is within 23:00-07:00 (before midnight)
        let before_midnight = chrono::NaiveDate::from_ymd_opt(2026, 3, 22)
            .unwrap()
            .and_hms_opt(23, 30, 0)
            .unwrap()
            .and_utc();
        assert!(is_in_active_window(&schedule, &before_midnight));

        // 12:00 is outside 23:00-07:00
        let noon = chrono::NaiveDate::from_ymd_opt(2026, 3, 22)
            .unwrap()
            .and_hms_opt(12, 0, 0)
            .unwrap()
            .and_utc();
        assert!(!is_in_active_window(&schedule, &noon));

        // 07:00 is at the boundary (exclusive end)
        let boundary = chrono::NaiveDate::from_ymd_opt(2026, 3, 22)
            .unwrap()
            .and_hms_opt(7, 0, 0)
            .unwrap()
            .and_utc();
        assert!(!is_in_active_window(&schedule, &boundary));
    }

    // -- Health sample tests -----------------------------------------------------

    fn make_sample(
        hostname: &str,
        sampled_at: &str,
        cpu: f64,
        mem_pct: f64,
    ) -> crate::health::HealthSample {
        crate::health::HealthSample {
            id: Uuid::now_v7().to_string(),
            hostname: hostname.to_string(),
            sampled_at: sampled_at.to_string(),
            cpu_usage_pct: cpu,
            load_avg_1: Some(2.0),
            load_avg_5: Some(1.5),
            load_avg_15: Some(1.0),
            cpu_core_count: 8,
            mem_total_bytes: 16_000_000_000,
            mem_used_bytes: (16_000_000_000.0 * mem_pct / 100.0) as i64,
            mem_usage_pct: mem_pct,
            swap_total_bytes: Some(4_000_000_000),
            swap_used_bytes: Some(0),
            cpu_temp_celsius: Some(55.0),
            agents_active: 2,
            pressure: cpu.max(mem_pct),
        }
    }

    #[test]
    fn insert_and_retrieve_health_sample() {
        let db = test_db();
        let sample = make_sample("macbook", "2026-04-03T10:00:00Z", 45.0, 60.0);
        db.insert_health_sample(&sample).unwrap();

        let latest = db.get_latest_health("macbook").unwrap().unwrap();
        assert_eq!(latest.hostname, "macbook");
        assert!((latest.cpu_usage_pct - 45.0).abs() < f64::EPSILON);
        assert!((latest.mem_usage_pct - 60.0).abs() < f64::EPSILON);
        assert_eq!(latest.cpu_core_count, 8);
        assert_eq!(latest.agents_active, 2);
    }

    #[test]
    fn get_health_history_filters_by_hostname() {
        let db = test_db();
        db.insert_health_sample(&make_sample("macbook", "2026-04-03T10:00:00Z", 40.0, 50.0))
            .unwrap();
        db.insert_health_sample(&make_sample(
            "linux-box",
            "2026-04-03T10:00:01Z",
            20.0,
            30.0,
        ))
        .unwrap();
        db.insert_health_sample(&make_sample("macbook", "2026-04-03T10:00:02Z", 45.0, 55.0))
            .unwrap();

        let history = db
            .get_health_history("macbook", "2026-04-03T09:00:00Z")
            .unwrap();
        assert_eq!(history.len(), 2);
        for s in &history {
            assert_eq!(s.hostname, "macbook");
        }
    }

    #[test]
    fn get_health_all_hosts_returns_all() {
        let db = test_db();
        db.insert_health_sample(&make_sample("macbook", "2026-04-03T10:00:00Z", 40.0, 50.0))
            .unwrap();
        db.insert_health_sample(&make_sample(
            "linux-box",
            "2026-04-03T10:00:01Z",
            20.0,
            30.0,
        ))
        .unwrap();

        let all = db.get_health_all_hosts("2026-04-03T09:00:00Z").unwrap();
        assert_eq!(all.len(), 2);

        let hostnames: Vec<&str> = all.iter().map(|s| s.hostname.as_str()).collect();
        assert!(hostnames.contains(&"macbook"));
        assert!(hostnames.contains(&"linux-box"));
    }

    #[test]
    fn prune_removes_old_samples() {
        let db = test_db();
        db.insert_health_sample(&make_sample("macbook", "2026-04-01T10:00:00Z", 40.0, 50.0))
            .unwrap();
        db.insert_health_sample(&make_sample("macbook", "2026-04-03T10:00:00Z", 45.0, 55.0))
            .unwrap();

        let pruned = db.prune_health_samples("2026-04-02T00:00:00Z").unwrap();
        assert_eq!(pruned, 1);

        let remaining = db
            .get_health_history("macbook", "2026-04-01T00:00:00Z")
            .unwrap();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].sampled_at, "2026-04-03T10:00:00Z");
    }

    #[test]
    fn get_latest_health_returns_most_recent() {
        let db = test_db();
        db.insert_health_sample(&make_sample("macbook", "2026-04-03T10:00:00Z", 30.0, 40.0))
            .unwrap();
        db.insert_health_sample(&make_sample("macbook", "2026-04-03T10:05:00Z", 50.0, 60.0))
            .unwrap();
        db.insert_health_sample(&make_sample("macbook", "2026-04-03T10:02:00Z", 40.0, 50.0))
            .unwrap();

        let latest = db.get_latest_health("macbook").unwrap().unwrap();
        assert_eq!(latest.sampled_at, "2026-04-03T10:05:00Z");
        assert!((latest.cpu_usage_pct - 50.0).abs() < f64::EPSILON);
    }

    #[test]
    fn get_latest_health_returns_none_for_unknown_host() {
        let db = test_db();
        let result = db.get_latest_health("nonexistent").unwrap();
        assert!(result.is_none());
    }

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

    #[test]
    fn get_recent_reflections_with_embeddings_returns_only_embedded() {
        let db = test_db();
        // Insert two reflections; only give one an embedding.
        let r1 = db
            .insert_reflection("kelex", "has embedding", "self")
            .unwrap();
        let _r2 = db
            .insert_reflection("kelex", "no embedding", "self")
            .unwrap();

        let blob = vec![0u8; 256 * 4]; // 256 f32 zeros
        db.store_embedding(&r1.id, &blob).unwrap();

        let results = db
            .get_recent_reflections_with_embeddings("kelex", 10)
            .unwrap();
        assert_eq!(
            results.len(),
            1,
            "only the embedded reflection should appear"
        );
        assert_eq!(results[0].0, r1.id);
    }

    #[test]
    fn get_recent_reflections_with_embeddings_respects_repo_scope() {
        let db = test_db();
        let r_kelex = db.insert_reflection("kelex", "kelex text", "self").unwrap();
        let r_rafters = db
            .insert_reflection("rafters", "rafters text", "self")
            .unwrap();

        let blob = vec![0u8; 256 * 4];
        db.store_embedding(&r_kelex.id, &blob).unwrap();
        db.store_embedding(&r_rafters.id, &blob).unwrap();

        let kelex_results = db
            .get_recent_reflections_with_embeddings("kelex", 10)
            .unwrap();
        assert_eq!(kelex_results.len(), 1);
        assert_eq!(kelex_results[0].0, r_kelex.id);
    }

    #[test]
    fn get_recent_reflections_with_embeddings_respects_limit() {
        let db = test_db();
        let blob = vec![0u8; 256 * 4];

        for i in 0..5 {
            let r = db
                .insert_reflection("legion", &format!("reflection {i}"), "self")
                .unwrap();
            db.store_embedding(&r.id, &blob).unwrap();
        }

        let results = db
            .get_recent_reflections_with_embeddings("legion", 3)
            .unwrap();
        assert_eq!(results.len(), 3);
    }

    #[test]
    fn quality_gate_insert_and_lookup() {
        let db = test_db();
        let row = db
            .record_quality_gate(
                "feat/test-branch",
                "abc1234def5678",
                "legion-simplify",
                "clean",
                0,
                None,
            )
            .unwrap();
        assert!(!row.id.is_empty());
        assert_eq!(row.branch, "feat/test-branch");
        assert_eq!(row.commit_hash, "abc1234def5678");
        assert_eq!(row.skill, "legion-simplify");
        assert_eq!(row.result, "clean");
        assert_eq!(row.findings_count, 0);
        assert!(row.details.is_none());

        let fetched = db
            .get_quality_gate("abc1234def5678", "legion-simplify")
            .unwrap();
        assert!(fetched.is_some());
        let fetched = fetched.unwrap();
        assert_eq!(fetched.id, row.id);
        assert_eq!(fetched.result, "clean");
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
        db.record_quality_gate("main", "abc1234", "legion-simplify", "clean", 0, None)
            .unwrap();
        // Different skill on the same commit should not match.
        let result = db.get_quality_gate("abc1234", "legion-review").unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn quality_gate_multiple_skills_on_same_commit() {
        let db = test_db();
        let hash = "deadbeef12345";
        db.record_quality_gate("main", hash, "legion-simplify", "clean", 0, None)
            .unwrap();
        db.record_quality_gate("main", hash, "legion-review", "issues", 2, Some("{}"))
            .unwrap();

        let simplify = db
            .get_quality_gate(hash, "legion-simplify")
            .unwrap()
            .expect("simplify gate should exist");
        assert_eq!(simplify.result, "clean");

        let review = db
            .get_quality_gate(hash, "legion-review")
            .unwrap()
            .expect("review gate should exist");
        assert_eq!(review.result, "issues");
        assert_eq!(review.findings_count, 2);
    }

    #[test]
    fn quality_gate_reruns_return_most_recent() {
        let db = test_db();
        let hash = "cafecafe99";
        // First run: issues found.
        db.record_quality_gate("main", hash, "legion-simplify", "issues", 3, None)
            .unwrap();
        // Second run after fixing: clean.
        db.record_quality_gate("main", hash, "legion-simplify", "clean", 0, None)
            .unwrap();

        let gate = db
            .get_quality_gate(hash, "legion-simplify")
            .unwrap()
            .expect("gate should exist");
        assert_eq!(
            gate.result, "clean",
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
                "issues",
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
}
