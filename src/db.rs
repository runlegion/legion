use std::path::Path;

use chrono::{Timelike, Utc};
use rusqlite::{Connection, OptionalExtension};
use uuid::Uuid;

use crate::error::{LegionError, Result};
use crate::sync::{
    CardDelta, ReflectionDelta, ScheduleDelta, UncertaintyCalibrationSnapshotDelta,
    UncertaintyPredictionDelta,
};

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
    pub(crate) conn: Connection,
}

/// A single stored reflection tied to a repository.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Reflection {
    pub id: String,
    pub repo: String,
    pub text: String,
    pub created_at: String,
    pub updated_at: Option<String>,
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
/// (id, repo, text, created_at, updated_at, audience, domain, tags, recall_count,
///  last_recalled_at, parent_id).
fn map_reflection_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Reflection> {
    Ok(Reflection {
        id: row.get(0)?,
        repo: row.get(1)?,
        text: row.get(2)?,
        created_at: row.get(3)?,
        updated_at: row.get(4)?,
        audience: row.get(5)?,
        domain: row.get(6)?,
        tags: row.get(7)?,
        recall_count: row.get(8)?,
        last_recalled_at: row.get(9)?,
        parent_id: row.get(10)?,
    })
}

/// Map a database row to a RateLimitSample struct. Shared by every query
/// that selects the canonical column order
/// (id, hostname, session_id, sampled_at, five_hour_pct, five_hour_resets_at,
///  seven_day_pct, seven_day_resets_at, model).
fn map_rate_limit_sample_row(
    row: &rusqlite::Row<'_>,
) -> rusqlite::Result<crate::statusline::RateLimitSample> {
    Ok(crate::statusline::RateLimitSample {
        id: row.get(0)?,
        hostname: row.get(1)?,
        session_id: row.get(2)?,
        sampled_at: row.get(3)?,
        five_hour_pct: row.get(4)?,
        five_hour_resets_at: row.get(5)?,
        seven_day_pct: row.get(6)?,
        seven_day_resets_at: row.get(7)?,
        model: row.get(8)?,
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
        updated_at: row.get(9)?,
        active_start: row.get(10)?,
        active_end: row.get(11)?,
    })
}

/// Optional metadata for a new reflection (Phase 2.0 Synapse fields).
#[derive(Default)]
pub struct ReflectionMeta {
    pub domain: Option<String>,
    pub tags: Option<String>,
    pub parent_id: Option<String>,
}

/// TTL hours for design or architecture posts.
const TTL_DESIGN_HOURS: i64 = 14 * 24;
/// TTL hours for signal-shaped posts (text starts with `@`).
const TTL_SIGNAL_HOURS: i64 = 7 * 24;
/// TTL hours for default broadcast posts.
const TTL_DEFAULT_HOURS: i64 = 48;

/// Compute (expires_at, evergreen) for a stored reflection (#376).
///
/// Non-team rows return (None, false): decay applies only to bullpen posts,
/// not to private reflections recalled by embedding similarity.
///
/// Team rows resolve in priority order:
/// 1. domain == "identity" or tags contains "doctrine" -> evergreen
/// 2. tags contains "design" or "architecture" -> 14 days
/// 3. text starts with `@` (directed signal or @all broadcast) -> 7 days
/// 4. default -> 48 hours
fn compute_post_ttl(
    audience: &str,
    domain: Option<&str>,
    tags: Option<&str>,
    text: &str,
    created_at: chrono::DateTime<Utc>,
) -> (Option<String>, bool) {
    if audience != "team" {
        return (None, false);
    }
    let tag_list: Vec<&str> = tags
        .map(|t| t.split(',').map(str::trim).collect())
        .unwrap_or_default();
    let has_tag = |needle: &str| tag_list.contains(&needle);

    if domain == Some("identity") || has_tag("doctrine") {
        return (None, true);
    }
    let hours = if has_tag("design") || has_tag("architecture") {
        TTL_DESIGN_HOURS
    } else if text.starts_with('@') {
        TTL_SIGNAL_HOURS
    } else {
        TTL_DEFAULT_HOURS
    };
    let expires = created_at + chrono::Duration::hours(hours);
    (Some(expires.to_rfc3339()), false)
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
    pub updated_at: Option<String>,
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

        // Migration: add last_read_id to board_reads (#400).
        //
        // Without an id alongside the timestamp, the MCP notifier's
        // per-recipient cursor cannot disambiguate two rows that share a
        // created_at -- the strict-`>` comparator in `get_board_posts_since`
        // is `(created_at > ?1) OR (created_at = ?1 AND id > ?2)`, and
        // `?2 = ""` (the empty seed) makes any non-empty id satisfy the
        // tie-break and re-emits the just-delivered row on every boot. The
        // empty-string default is correct for existing rows: it means "no
        // id known, treat the timestamp as the only ordering signal" --
        // identical to the pre-#400 behaviour the row was inserted under.
        if !Self::has_column(conn, "board_reads", "last_read_id")? {
            conn.execute_batch(
                "ALTER TABLE board_reads ADD COLUMN last_read_id TEXT NOT NULL DEFAULT '';",
            )?;
        }

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

        // Migration 13: Soft delete support for multi-node sync (#245).
        // Adds deleted_at column to syncable tables. Rows with deleted_at set
        // are excluded from normal queries but included in sync deltas.
        if !Self::has_column(conn, "reflections", "deleted_at")? {
            conn.execute_batch("ALTER TABLE reflections ADD COLUMN deleted_at TEXT;")?;
        }
        if !Self::has_column(conn, "tasks", "deleted_at")? {
            conn.execute_batch("ALTER TABLE tasks ADD COLUMN deleted_at TEXT;")?;
        }
        if !Self::has_column(conn, "schedules", "deleted_at")? {
            conn.execute_batch("ALTER TABLE schedules ADD COLUMN deleted_at TEXT;")?;
        }

        // Migration 14: Add updated_at for LWW conflict resolution (#255).
        // Required for multi-node sync to determine which version wins when
        // the same row is modified on different nodes.
        if !Self::has_column(conn, "reflections", "updated_at")? {
            conn.execute_batch(
                "ALTER TABLE reflections ADD COLUMN updated_at TEXT;
                 UPDATE reflections SET updated_at = created_at WHERE updated_at IS NULL;",
            )?;
        }
        if !Self::has_column(conn, "schedules", "updated_at")? {
            conn.execute_batch(
                "ALTER TABLE schedules ADD COLUMN updated_at TEXT;
                 UPDATE schedules SET updated_at = created_at WHERE updated_at IS NULL;",
            )?;
        }

        // Migration 15: Partial indexes for soft-deleted rows (#256).
        // Most queries filter by deleted_at IS NULL. Partial indexes exclude
        // tombstones, reducing index size and improving query performance.
        conn.execute_batch(
            "CREATE INDEX IF NOT EXISTS idx_reflections_repo_live \
                 ON reflections(repo) WHERE deleted_at IS NULL;
             CREATE INDEX IF NOT EXISTS idx_reflections_audience_live \
                 ON reflections(audience, created_at) WHERE deleted_at IS NULL;
             CREATE INDEX IF NOT EXISTS idx_tasks_to_live \
                 ON tasks(to_repo, status) WHERE deleted_at IS NULL;
             CREATE INDEX IF NOT EXISTS idx_tasks_from_live \
                 ON tasks(from_repo) WHERE deleted_at IS NULL;
             CREATE INDEX IF NOT EXISTS idx_schedules_repo_live \
                 ON schedules(repo) WHERE deleted_at IS NULL;",
        )?;

        // Migration 16: Rate limit and usage samples for pillar-2 scheduler (#287).
        // Populated by `legion statusline` on every Claude Code render. Both
        // tables carry deleted_at + updated_at for smugglr content-hash sync.
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS rate_limit_samples (
                id TEXT PRIMARY KEY,
                hostname TEXT NOT NULL,
                session_id TEXT NOT NULL,
                sampled_at TEXT NOT NULL,
                five_hour_pct REAL,
                five_hour_resets_at INTEGER,
                seven_day_pct REAL,
                seven_day_resets_at INTEGER,
                model TEXT,
                deleted_at TEXT,
                updated_at TEXT
            );
            CREATE INDEX IF NOT EXISTS idx_rate_limit_samples_sampled \
                ON rate_limit_samples(sampled_at);
            CREATE INDEX IF NOT EXISTS idx_rate_limit_samples_host \
                ON rate_limit_samples(hostname);
            CREATE INDEX IF NOT EXISTS idx_rate_limit_samples_live \
                ON rate_limit_samples(hostname, sampled_at) WHERE deleted_at IS NULL;

            CREATE TABLE IF NOT EXISTS usage_samples (
                id TEXT PRIMARY KEY,
                hostname TEXT NOT NULL,
                session_id TEXT NOT NULL,
                turn_index INTEGER,
                model TEXT,
                input_tokens INTEGER NOT NULL DEFAULT 0,
                output_tokens INTEGER NOT NULL DEFAULT 0,
                cache_write_tokens INTEGER NOT NULL DEFAULT 0,
                cache_read_tokens INTEGER NOT NULL DEFAULT 0,
                effective_tokens INTEGER NOT NULL,
                error_bytes INTEGER NOT NULL DEFAULT 0,
                sampled_at TEXT NOT NULL,
                deleted_at TEXT,
                updated_at TEXT
            );
            CREATE INDEX IF NOT EXISTS idx_usage_samples_sampled \
                ON usage_samples(sampled_at);
            CREATE INDEX IF NOT EXISTS idx_usage_samples_host \
                ON usage_samples(hostname);
            CREATE INDEX IF NOT EXISTS idx_usage_samples_session \
                ON usage_samples(session_id);
            CREATE INDEX IF NOT EXISTS idx_usage_samples_live \
                ON usage_samples(hostname, sampled_at) WHERE deleted_at IS NULL;",
        )?;

        // Migration 17: Persona wake leases for cluster-wide wake coordination (#308).
        //
        // When a signal arrives addressed to a persona (either `--to P` or `--to all`),
        // watch acquires a lease keyed by (persona_id, signal_id) before spawning. Other
        // nodes (or later poll cycles on the same node) see the lease is held and skip
        // the wake. Heartbeats keep the lease fresh; crashes release via TTL.
        //
        // `deleted_at` + `updated_at` carry the usual LWW semantics for smugglr sync.
        // `expires_at` is a denormalized scalar for cheap "is this lease still live"
        // filters without constructing a duration against `heartbeat_at` at query time.
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS persona_wake_leases (
                persona_id TEXT NOT NULL,
                signal_id TEXT NOT NULL,
                acquired_by_host TEXT NOT NULL,
                acquired_at TEXT NOT NULL,
                heartbeat_at TEXT NOT NULL,
                expires_at TEXT NOT NULL,
                deleted_at TEXT,
                updated_at TEXT NOT NULL,
                PRIMARY KEY (persona_id, signal_id)
            );
            CREATE INDEX IF NOT EXISTS idx_persona_wake_leases_persona \
                ON persona_wake_leases(persona_id) WHERE deleted_at IS NULL;
            CREATE INDEX IF NOT EXISTS idx_persona_wake_leases_expires \
                ON persona_wake_leases(expires_at) WHERE deleted_at IS NULL;",
        )?;

        // Migration 17: SCIP indexes for code intelligence (#278).
        // One active row per (repo, lang) holds the protobuf blob legion
        // queries against. content_hash is SHA-256 hex of the blob; an
        // upsert with an unchanged hash short-circuits to bumping
        // updated_at without rewriting the blob bytes.
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS scip_indexes (
                id TEXT PRIMARY KEY,
                repo TEXT NOT NULL,
                lang TEXT NOT NULL,
                content_hash TEXT NOT NULL,
                blob BLOB NOT NULL,
                updated_at TEXT NOT NULL,
                deleted_at TEXT
            );
            CREATE UNIQUE INDEX IF NOT EXISTS idx_scip_indexes_repo_lang_live
                ON scip_indexes(repo, lang) WHERE deleted_at IS NULL;
            CREATE INDEX IF NOT EXISTS idx_scip_indexes_repo
                ON scip_indexes(repo) WHERE deleted_at IS NULL;",
        )?;

        // Migration 18: Bullpen post decay (#376).
        //
        // Stale bullpen posts were landing in agent context via the channel
        // notification path, costing tokens to read and reason about threads
        // long since resolved. Filter at injection: every team-audience post
        // gets either an `expires_at` timestamp (computed at insert from
        // domain/tags/text shape) or `evergreen=1` (identity/doctrine).
        // Read-side queries filter out expired non-evergreen rows. Posts stay
        // in the DB; they are simply invisible to the agent surface.
        let new_expires = !Self::has_column(conn, "reflections", "expires_at")?;
        let new_evergreen = !Self::has_column(conn, "reflections", "evergreen")?;
        if new_expires {
            conn.execute_batch("ALTER TABLE reflections ADD COLUMN expires_at TEXT;")?;
        }
        if new_evergreen {
            conn.execute_batch(
                "ALTER TABLE reflections ADD COLUMN evergreen INTEGER NOT NULL DEFAULT 0;",
            )?;
        }
        conn.execute_batch(
            "CREATE INDEX IF NOT EXISTS idx_reflections_team_expires \
                 ON reflections(audience, expires_at) \
                 WHERE audience = 'team' AND archived_at IS NULL AND deleted_at IS NULL;",
        )?;

        // Backfill: compute TTL for every team row that lacks one. Done in
        // Rust (not pure SQL) so the same `compute_post_ttl` rule used at
        // insert applies to historical rows -- no drift between migration
        // logic and runtime logic.
        if new_expires || new_evergreen {
            // Backfill row tuple: (id, created_at, audience, domain, tags, text).
            type BackfillRow = (
                String,
                String,
                String,
                Option<String>,
                Option<String>,
                String,
            );
            let mut stmt = conn.prepare(
                "SELECT id, created_at, audience, domain, tags, text \
                 FROM reflections \
                 WHERE audience = 'team' AND expires_at IS NULL AND evergreen = 0",
            )?;
            let rows: Vec<BackfillRow> = stmt
                .query_map([], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, Option<String>>(3)?,
                        row.get::<_, Option<String>>(4)?,
                        row.get::<_, String>(5)?,
                    ))
                })?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            drop(stmt);

            for (id, created_at, audience, domain, tags, text) in rows {
                let parsed = chrono::DateTime::parse_from_rfc3339(&created_at)
                    .map(|d| d.with_timezone(&Utc))
                    .unwrap_or_else(|_| Utc::now());
                let (expires_at, evergreen) =
                    compute_post_ttl(&audience, domain.as_deref(), tags.as_deref(), &text, parsed);
                conn.execute(
                    "UPDATE reflections SET expires_at = ?1, evergreen = ?2 WHERE id = ?3",
                    rusqlite::params![expires_at, evergreen as i32, id],
                )?;
            }
        }

        // Migration 19: Signal/post resolution marker (#362).
        //
        // Threads converge on a decision but the originating signal stays
        // open. Agents who did not see the convergence walk the thread again,
        // posting the same conclusion. `resolved_at` + optional
        // `resolved_by_reflection_id` mark a thread done. Resolved rows are
        // filtered out of read paths by default; agents do not see them.
        // Operator review uses `--include-resolved`.
        if !Self::has_column(conn, "reflections", "resolved_at")? {
            conn.execute_batch("ALTER TABLE reflections ADD COLUMN resolved_at TEXT;")?;
        }
        if !Self::has_column(conn, "reflections", "resolved_by_reflection_id")? {
            conn.execute_batch(
                "ALTER TABLE reflections ADD COLUMN resolved_by_reflection_id TEXT;",
            )?;
        }
        conn.execute_batch(
            "CREATE INDEX IF NOT EXISTS idx_reflections_team_unresolved \
                 ON reflections(audience, resolved_at) \
                 WHERE audience = 'team' AND archived_at IS NULL AND deleted_at IS NULL;",
        )?;

        // Migration 20: Agent session log (#389).
        //
        // Watch spawns an agent for one or more signals, marks them handled
        // at spawn time, then has no further accounting for what the agent
        // did. Productive vs unproductive sessions look identical on the
        // outside (exit_code 0). This table records every spawn outcome so
        // `legion status` can surface "shingle: 3 sessions today, 0
        // produced output." Classification happens at reap, comparing
        // bullpen posts and reflections written within the session window.
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS agent_session_log (
                session_id TEXT PRIMARY KEY,
                recipient TEXT NOT NULL,
                signal_ids TEXT NOT NULL,
                spawn_at TEXT NOT NULL,
                exit_at TEXT NOT NULL,
                exit_status TEXT NOT NULL,
                outcome TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_agent_session_log_recipient_exit \
                ON agent_session_log(recipient, exit_at);",
        )?;

        // Migration 21: Documents table for the coordination substrate
        // (#456, child of #455).
        //
        // Stores spec / NFR / blueprint / persona / journey / etc as rows
        // with structured meta columns hoisted from a JSON payload. Hot
        // pool by default; archived_at populates when work referencing
        // the doc completes (kanban-spec linkage in a later child issue).
        //
        // The payload column holds the validated JSON for the document
        // type. Type-specific schema validation at INSERT happens in a
        // sibling child issue under #455 once vault's schemas land. For
        // #456 the foundation ships; validator is a stub that accepts
        // any JSON.
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS documents (
                id TEXT PRIMARY KEY,
                type TEXT NOT NULL,
                surface TEXT,
                status TEXT NOT NULL DEFAULT 'draft',
                priority TEXT,
                owner TEXT NOT NULL,
                payload TEXT NOT NULL,
                archived_at TEXT,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                deleted_at TEXT
            );
            CREATE INDEX IF NOT EXISTS idx_documents_type
                ON documents(type) WHERE archived_at IS NULL AND deleted_at IS NULL;
            CREATE INDEX IF NOT EXISTS idx_documents_surface
                ON documents(surface) WHERE archived_at IS NULL AND deleted_at IS NULL;
            CREATE INDEX IF NOT EXISTS idx_documents_owner
                ON documents(owner) WHERE archived_at IS NULL AND deleted_at IS NULL;
            CREATE INDEX IF NOT EXISTS idx_documents_status
                ON documents(status) WHERE archived_at IS NULL AND deleted_at IS NULL;
            CREATE INDEX IF NOT EXISTS idx_documents_archived
                ON documents(archived_at, type) WHERE archived_at IS NOT NULL AND deleted_at IS NULL;",
        )?;

        // Migration 22: Uncertainty engine tables (#355, child of #354).
        //
        // Pillar 2 turns features from SCIP + task descriptions into a
        // calibrated prediction with a confidence bucket, witnesses the
        // outcome from usage + PR merges, and rolls calibration snapshots
        // nightly. Lifecycle: emitted -> witnessed -> calibrated -> orphaned
        // -> retired. The orphan state is load-bearing -- silence is its own
        // state, counted under the Brier uncertainty term (not reliability),
        // so unwitnessed predictions do not poison the reliability score.
        //
        // Post-correction notes from platform (Whatsonyourmind review on
        // legion#354, ack 019de0ac):
        //
        //  - bucket_lower / bucket_upper are quantile-derived (equal-frequency,
        //    10 per cohort) -- schema unchanged, but the calibration roller
        //    writes quantile bounds rather than fixed 0.1 widths.
        //  - actual_correctness stores the Empirical-Bayes Beta-posterior
        //    shrunk value used for visible calibration; actual_correctness_raw
        //    stores the unshrunk cell average for audit and back-out.
        //  - Reference: Brocker (2009), reliability/sufficiency decomposition.
        //
        // updated_at + deleted_at columns exist for smugglr LWW sync
        // (mirrors reflections / tasks / schedules).
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS uncertainty_prediction (
                id TEXT PRIMARY KEY,
                surface TEXT NOT NULL,
                feature_key TEXT NOT NULL,
                input_fingerprint TEXT NOT NULL,
                model TEXT NOT NULL,
                model_version TEXT NOT NULL,
                claimed_confidence REAL NOT NULL,
                prediction_payload TEXT NOT NULL,
                state TEXT NOT NULL DEFAULT 'emitted',
                outcome_label TEXT,
                outcome_payload TEXT,
                outcome_correctness REAL,
                cohort_key TEXT NOT NULL,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                witnessed_at TEXT,
                orphan_after TEXT,
                deleted_at TEXT
            );
            CREATE INDEX IF NOT EXISTS idx_uncertainty_prediction_cohort
                ON uncertainty_prediction(cohort_key) WHERE deleted_at IS NULL;
            CREATE INDEX IF NOT EXISTS idx_uncertainty_prediction_surface
                ON uncertainty_prediction(surface) WHERE deleted_at IS NULL;
            CREATE INDEX IF NOT EXISTS idx_uncertainty_prediction_state
                ON uncertainty_prediction(state) WHERE deleted_at IS NULL;
            CREATE INDEX IF NOT EXISTS idx_uncertainty_prediction_orphan_sweep
                ON uncertainty_prediction(state, orphan_after) WHERE deleted_at IS NULL;

            CREATE TABLE IF NOT EXISTS uncertainty_calibration_snapshot (
                id TEXT PRIMARY KEY,
                cohort_key TEXT NOT NULL,
                bucket_lower REAL NOT NULL,
                bucket_upper REAL NOT NULL,
                claimed_confidence REAL NOT NULL,
                actual_correctness REAL NOT NULL,
                actual_correctness_raw REAL NOT NULL,
                prediction_count INTEGER NOT NULL,
                orphan_count INTEGER NOT NULL,
                brier_score REAL NOT NULL,
                computed_at TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                deleted_at TEXT
            );
            CREATE INDEX IF NOT EXISTS idx_uncertainty_calibration_cohort
                ON uncertainty_calibration_snapshot(cohort_key) WHERE deleted_at IS NULL;
            CREATE INDEX IF NOT EXISTS idx_uncertainty_calibration_computed
                ON uncertainty_calibration_snapshot(computed_at) WHERE deleted_at IS NULL;",
        )?;

        // Migration 23: wake_attempts -- ACID substrate for the watch PTY
        // migration (#487, part of #495).
        //
        // Each row represents one wake spawn from queue through terminal
        // classification. The FSM enforced in Rust
        // (`wake_attempts::WakeAttemptState::can_transition_to`) and
        // mirrored in every UPDATE here keeps `state = from` in the WHERE
        // clause so a sync-resolved late-loser cannot regress a peer's
        // already-terminal row.
        //
        // persona_wake_leases (Migration 17) keeps its TTL-based mutual
        // exclusion role; wake_attempts records what actually happened
        // on each individual spawn so the reaper has a persistent,
        // cluster-visible work item to operate on.
        //
        // signal_ids stored as a JSON array TEXT column to avoid a
        // wake_attempt_signals join table -- N is small (a wake batches
        // a handful of signals at most) and the column is opaque to the
        // SQL layer.
        //
        // Indices target the two hot read paths: crash recovery scans
        // by (acquired_by_host, state) on startup, and operator history
        // queries scan by (persona_id, spawned_at DESC).
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS wake_attempts (
                attempt_id TEXT PRIMARY KEY,
                persona_id TEXT NOT NULL,
                repo_name TEXT NOT NULL,
                signal_ids TEXT NOT NULL,
                state TEXT NOT NULL,
                acquired_by_host TEXT,
                acquired_at TEXT,
                spawned_pid INTEGER,
                spawned_at TEXT,
                exit_observed_at TEXT,
                exited_at TEXT,
                exit_status TEXT,
                outcome TEXT,
                deleted_at TEXT,
                updated_at TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_wake_attempts_host_state
                ON wake_attempts(acquired_by_host, state)
                WHERE deleted_at IS NULL;
            CREATE INDEX IF NOT EXISTS idx_wake_attempts_persona_recent
                ON wake_attempts(persona_id, spawned_at DESC)
                WHERE deleted_at IS NULL;",
        )?;

        // #524: per-agent weekly autonomy budget. One row per repo; the window
        // rolls over lazily on read (see autonomy::rolled_over).
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS autonomy_budget (
                repo TEXT PRIMARY KEY,
                window_start TEXT NOT NULL,
                spent INTEGER NOT NULL DEFAULT 0,
                ceiling INTEGER NOT NULL,
                updated_at TEXT NOT NULL
            );",
        )?;

        // Migration 24: watch heartbeat (#581).
        //
        // One row per host, keyed by hostname. The daemon upserts this row on
        // every health_interval tick so an operator can run `legion watch status`
        // and know whether the daemon is alive, stale, or has never written a beat.
        // Singleton-per-host with no UUID id column; the primary key IS the host.
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS watch_heartbeat (
                host TEXT PRIMARY KEY,
                pid INTEGER NOT NULL,
                version TEXT NOT NULL,
                repo_count INTEGER NOT NULL,
                last_spawn_summary TEXT,
                updated_at TEXT NOT NULL
            );",
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
        let created_at_dt = Utc::now();
        let created_at = created_at_dt.to_rfc3339();
        let (expires_at, evergreen) = compute_post_ttl(
            audience,
            meta.domain.as_deref(),
            meta.tags.as_deref(),
            text,
            created_at_dt,
        );

        self.conn.execute(
            "INSERT INTO reflections (id, repo, text, created_at, updated_at, audience, domain, tags, parent_id, expires_at, evergreen) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            rusqlite::params![
                &id, repo, text, &created_at, &created_at, audience,
                &meta.domain, &meta.tags, &meta.parent_id,
                expires_at, evergreen as i32,
            ],
        )?;

        Ok(Reflection {
            id,
            repo: repo.to_owned(),
            text: text.to_owned(),
            created_at: created_at.clone(),
            updated_at: Some(created_at),
            audience: audience.to_owned(),
            domain: meta.domain.clone(),
            tags: meta.tags.clone(),
            recall_count: 0,
            last_recalled_at: None,
            parent_id: meta.parent_id.clone(),
        })
    }

    /// Mark a bullpen post or signal as resolved (#362).
    ///
    /// Records `resolved_at = now` and an optional reflection id pointing at
    /// the reflection that holds the team's converged decision. Idempotent:
    /// re-resolving a row updates `resolved_at` to the latest call but
    /// preserves the reflection link unless a new one is provided.
    ///
    /// Returns `Ok(false)` when the id does not exist or is already
    /// soft-deleted; `Ok(true)` when a row was updated.
    pub fn resolve_post(
        &self,
        post_id: &str,
        resolved_by_reflection_id: Option<&str>,
    ) -> Result<bool> {
        let now = Utc::now().to_rfc3339();
        let rows = self.conn.execute(
            "UPDATE reflections \
             SET resolved_at = ?1, \
                 resolved_by_reflection_id = COALESCE(?2, resolved_by_reflection_id), \
                 updated_at = ?1 \
             WHERE id = ?3 AND deleted_at IS NULL",
            rusqlite::params![&now, resolved_by_reflection_id, post_id],
        )?;
        Ok(rows > 0)
    }

    /// Outcome of a watch-spawned agent session (#389).
    ///
    /// "Productive" means the recipient posted to the bullpen or stored a
    /// reflection within the session window. "Unproductive" means the
    /// session exited cleanly but produced no observable artifact.
    /// "Errored" means the process exited non-zero.
    ///
    /// Strings used directly as the `outcome` column value.
    pub const OUTCOME_PRODUCTIVE: &'static str = "productive";
    pub const OUTCOME_UNPRODUCTIVE: &'static str = "unproductive";
    pub const OUTCOME_ERRORED: &'static str = "errored";

    /// Classify whether a watch-spawned agent session produced observable
    /// work for any of its signal_ids (#389). Productive iff:
    ///   - the recipient posted a bullpen entry within (spawn_at, exit_at), OR
    ///   - the recipient stored a reflection whose `parent_id` matches any of
    ///     the spawn's signal_ids within the window.
    ///
    /// Window endpoints are inclusive at start, exclusive at end so a
    /// reflection committed in the same RFC3339 millisecond as exit is
    /// still attributed to the session.
    pub fn classify_session(
        &self,
        recipient: &str,
        signal_ids: &[String],
        spawn_at: &str,
        exit_at: &str,
    ) -> Result<bool> {
        let bullpen_count: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM reflections \
             WHERE repo = ?1 AND audience = 'team' AND deleted_at IS NULL \
               AND created_at >= ?2 AND created_at <= ?3",
            rusqlite::params![recipient, spawn_at, exit_at],
            |row| row.get(0),
        )?;
        if bullpen_count > 0 {
            return Ok(true);
        }
        if signal_ids.is_empty() {
            return Ok(false);
        }
        // Reflection-with-parent_id linking back to any tracked signal.
        // Build a parameter list dynamically; signal_ids is bounded by
        // the wake batch (legion watch only ever wakes a few at once).
        let placeholders = (0..signal_ids.len())
            .map(|i| format!("?{}", i + 4))
            .collect::<Vec<_>>()
            .join(",");
        let sql = format!(
            "SELECT COUNT(*) FROM reflections \
             WHERE repo = ?1 AND deleted_at IS NULL \
               AND created_at >= ?2 AND created_at <= ?3 \
               AND parent_id IN ({placeholders})"
        );
        let mut params: Vec<Box<dyn rusqlite::ToSql>> = vec![
            Box::new(recipient.to_string()),
            Box::new(spawn_at.to_string()),
            Box::new(exit_at.to_string()),
        ];
        for id in signal_ids {
            params.push(Box::new(id.clone()));
        }
        let param_refs: Vec<&dyn rusqlite::ToSql> = params.iter().map(|p| p.as_ref()).collect();
        let reflection_count: i64 =
            self.conn
                .query_row(sql.as_str(), param_refs.as_slice(), |row| row.get(0))?;
        Ok(reflection_count > 0)
    }

    /// Persist an agent session outcome (#389). One row per spawn-and-exit.
    #[allow(clippy::too_many_arguments)]
    pub fn record_session_outcome(
        &self,
        session_id: &str,
        recipient: &str,
        signal_ids: &[String],
        spawn_at: &str,
        exit_at: &str,
        exit_status: &str,
        outcome: &str,
    ) -> Result<()> {
        let signal_ids_json = serde_json::to_string(signal_ids).map_err(|e| {
            LegionError::Database(rusqlite::Error::ToSqlConversionFailure(Box::new(e)))
        })?;
        self.conn.execute(
            "INSERT OR REPLACE INTO agent_session_log \
                 (session_id, recipient, signal_ids, spawn_at, exit_at, exit_status, outcome) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            rusqlite::params![
                session_id,
                recipient,
                signal_ids_json,
                spawn_at,
                exit_at,
                exit_status,
                outcome
            ],
        )?;
        Ok(())
    }

    /// Per-recipient session counts since the given timestamp (#389).
    /// Returns (recipient, total, productive, unproductive, errored, last_unproductive_signal_id).
    /// Used by `legion status` to surface "shingle: 3 sessions today, 0 productive."
    #[allow(clippy::type_complexity)]
    pub fn agent_session_counts_since(
        &self,
        since: &str,
    ) -> Result<Vec<(String, i64, i64, i64, i64, Option<String>, Option<String>)>> {
        let mut stmt = self.conn.prepare(
            "SELECT recipient, \
                    COUNT(*) as total, \
                    SUM(CASE WHEN outcome = 'productive' THEN 1 ELSE 0 END) as productive, \
                    SUM(CASE WHEN outcome = 'unproductive' THEN 1 ELSE 0 END) as unproductive, \
                    SUM(CASE WHEN outcome = 'errored' THEN 1 ELSE 0 END) as errored \
             FROM agent_session_log \
             WHERE exit_at >= ?1 \
             GROUP BY recipient \
             ORDER BY total DESC",
        )?;
        let core: Vec<(String, i64, i64, i64, i64)> = stmt
            .query_map([since], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, i64>(4)?,
                ))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(LegionError::Database)?;
        drop(stmt);

        let mut out = Vec::with_capacity(core.len());
        for (recipient, total, productive, unproductive, errored) in core {
            let last_unproductive: Option<(String, String)> = self
                .conn
                .query_row(
                    "SELECT signal_ids, exit_at FROM agent_session_log \
                     WHERE recipient = ?1 AND outcome = 'unproductive' AND exit_at >= ?2 \
                     ORDER BY exit_at DESC LIMIT 1",
                    rusqlite::params![&recipient, since],
                    |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
                )
                .optional()
                .map_err(LegionError::Database)?;
            let (last_signal, last_exit_at) = match last_unproductive {
                Some((ids_json, exit_at)) => {
                    let ids: Vec<String> = serde_json::from_str(&ids_json).unwrap_or_default();
                    (ids.into_iter().next(), Some(exit_at))
                }
                None => (None, None),
            };
            out.push((
                recipient,
                total,
                productive,
                unproductive,
                errored,
                last_signal,
                last_exit_at,
            ));
        }
        Ok(out)
    }

    /// Store an embedding BLOB for an existing reflection.
    pub fn store_embedding(&self, id: &str, embedding_bytes: &[u8]) -> Result<bool> {
        let now = Utc::now().to_rfc3339();
        let rows = self.conn.execute(
            "UPDATE reflections SET embedding = ?1, updated_at = ?3 WHERE id = ?2 AND deleted_at IS NULL",
            rusqlite::params![embedding_bytes, id, &now],
        )?;
        Ok(rows > 0)
    }

    /// Retrieve the embedding BLOB for a reflection, if it exists.
    pub fn get_embedding(&self, id: &str) -> Result<Option<Vec<u8>>> {
        let mut stmt = self
            .conn
            .prepare("SELECT embedding FROM reflections WHERE id = ?1 AND deleted_at IS NULL")?;
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

        let base = "SELECT id, embedding FROM reflections WHERE embedding IS NOT NULL AND deleted_at IS NULL";
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
            "SELECT id, text FROM reflections WHERE embedding IS NULL AND deleted_at IS NULL ORDER BY created_at DESC",
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
             WHERE repo = ?1 AND embedding IS NOT NULL AND deleted_at IS NULL \
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
            "UPDATE reflections SET recall_count = recall_count + 1, last_recalled_at = ?1, updated_at = ?1 WHERE id = ?2 AND deleted_at IS NULL",
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

    /// True if this reflection (live, not soft-deleted) participates in a
    /// chain -- either it has a parent or at least one live child. Used by
    /// `whoami` to decide whether to surface a `legion chain --id <id>`
    /// pointer without forcing callers to walk the full chain.
    pub fn is_in_chain(&self, id: &str) -> Result<bool> {
        let row: Option<i64> = self
            .conn
            .query_row(
                "SELECT 1 FROM reflections r \
                 WHERE r.id = ?1 AND r.deleted_at IS NULL \
                   AND (r.parent_id IS NOT NULL \
                        OR EXISTS (SELECT 1 FROM reflections c \
                                   WHERE c.parent_id = r.id AND c.deleted_at IS NULL))",
                [id],
                |row| row.get(0),
            )
            .optional()?;
        Ok(row.is_some())
    }

    /// Find the child reflection that follows the given parent ID.
    fn find_child(&self, parent_id: &str) -> Result<Option<String>> {
        let mut stmt = self.conn.prepare(
            "SELECT id FROM reflections WHERE parent_id = ?1 AND deleted_at IS NULL LIMIT 1",
        )?;
        let mut rows = stmt.query_map([parent_id], |row| row.get::<_, String>(0))?;
        match rows.next() {
            Some(row) => Ok(Some(row?)),
            None => Ok(None),
        }
    }

    /// Retrieve a single reflection by its ID.
    ///
    /// Returns `None` if no reflection exists with the given ID or if soft-deleted.
    pub fn get_reflection_by_id(&self, id: &str) -> Result<Option<Reflection>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, repo, text, created_at, updated_at, audience, domain, tags, recall_count, last_recalled_at, parent_id FROM reflections WHERE id = ?1 AND deleted_at IS NULL",
        )?;

        let mut rows = stmt.query_map([id], map_reflection_row)?;

        match rows.next() {
            Some(row) => Ok(Some(row?)),
            None => Ok(None),
        }
    }

    /// Read a reflection by id, applying an archive-mode filter (#457).
    /// Hot: returns None if archived_at IS NOT NULL.
    /// Cold: returns None if archived_at IS NULL.
    /// Both: same behavior as `get_reflection_by_id` (no archive filter).
    ///
    /// The deleted_at filter applies in all modes (soft-deleted rows are
    /// always hidden).
    pub fn get_reflection_by_id_in_mode(
        &self,
        id: &str,
        mode: crate::recall::ArchiveMode,
    ) -> Result<Option<Reflection>> {
        let where_clause = match mode {
            crate::recall::ArchiveMode::Hot => "AND archived_at IS NULL",
            crate::recall::ArchiveMode::Cold => "AND archived_at IS NOT NULL",
            crate::recall::ArchiveMode::Both => "",
        };
        let sql = format!(
            "SELECT id, repo, text, created_at, updated_at, audience, domain, tags, recall_count, last_recalled_at, parent_id \
             FROM reflections WHERE id = ?1 AND deleted_at IS NULL {where_clause}"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let mut rows = stmt.query_map([id], map_reflection_row)?;
        match rows.next() {
            Some(row) => Ok(Some(row?)),
            None => Ok(None),
        }
    }

    /// Permanently remove a reflection from the database by id.
    ///
    /// Returns the deleted reflection so the caller can confirm what was
    /// removed (repo, audience, first 80 chars of text, etc). Returns
    /// `LegionError::ReflectionNotFound` if the id does not match any
    /// row.
    ///
    /// This is a HARD delete from SQLite only. Callers must also call
    /// `SearchIndex::delete(id)` to remove the matching document from
    /// the tantivy index, or subsequent BM25 queries will still return
    /// the deleted reflection as a "ghost" until the next reindex.
    ///
    /// Destructive. No soft-delete, no undo. Used by `legion forget` to
    /// retire stale workaround reflections, bad reflections, or personal
    /// data that should not persist in the corpus.
    pub fn delete_reflection(&self, id: &str) -> Result<Reflection> {
        // Fetch first so we can return it and so a missing id produces a
        // clear error rather than a silent zero-row delete.
        let reflection = self
            .get_reflection_by_id(id)?
            .ok_or_else(|| LegionError::ReflectionNotFound(id.to_string()))?;

        let rows = self.conn.execute(
            "DELETE FROM reflections WHERE id = ?1",
            rusqlite::params![id],
        )?;
        if rows == 0 {
            // Race: reflection existed at the fetch above but was
            // deleted before our delete ran. Surface as NotFound rather
            // than success.
            return Err(LegionError::ReflectionNotFound(id.to_string()));
        }
        Ok(reflection)
    }

    /// Soft-delete a reflection by setting its deleted_at timestamp.
    ///
    /// Unlike `delete_reflection` (hard delete), this preserves the row for
    /// multi-node sync tombstone propagation. The row becomes invisible to
    /// normal queries but can still be synced to other nodes.
    #[allow(dead_code)] // Used by sync module in #248
    pub fn soft_delete_reflection(&self, id: &str) -> Result<bool> {
        let now = Utc::now().to_rfc3339();
        let rows = self.conn.execute(
            "UPDATE reflections SET deleted_at = ?1, updated_at = ?1 WHERE id = ?2 AND deleted_at IS NULL",
            rusqlite::params![now, id],
        )?;
        Ok(rows > 0)
    }

    /// Get reflection deltas for multi-node sync.
    ///
    /// Returns all reflections that have been modified or soft-deleted since
    /// the given timestamp. Used for delta synchronization between legion nodes.
    ///
    /// The query includes:
    /// - Live rows where updated_at > since (modifications)
    /// - Soft-deleted rows where deleted_at > since (tombstones)
    ///
    /// Excludes embedding column since each node computes its own embeddings.
    #[allow(dead_code)] // Used by sync broadcast in #248
    pub fn get_reflection_deltas_since(&self, since: &str) -> Result<Vec<ReflectionDelta>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, repo, text, created_at, updated_at, deleted_at, audience, domain, tags, \
             recall_count, last_recalled_at, parent_id \
             FROM reflections \
             WHERE updated_at > ?1 OR deleted_at > ?1 \
             ORDER BY COALESCE(updated_at, deleted_at) ASC",
        )?;

        let rows = stmt.query_map([since], |row| {
            Ok(ReflectionDelta {
                id: row.get(0)?,
                repo: row.get(1)?,
                text: row.get(2)?,
                created_at: row.get(3)?,
                updated_at: row.get(4)?,
                deleted_at: row.get(5)?,
                audience: row.get(6)?,
                domain: row.get(7)?,
                tags: row.get(8)?,
                recall_count: row.get(9)?,
                last_recalled_at: row.get(10)?,
                parent_id: row.get(11)?,
            })
        })?;

        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(LegionError::Database)
    }

    /// Get card deltas for multi-node sync.
    ///
    /// Returns all cards (tasks table) that have been modified or soft-deleted
    /// since the given timestamp. Used for delta synchronization between nodes.
    #[allow(dead_code)] // Used by sync broadcast in #249
    pub fn get_card_deltas_since(&self, since: &str) -> Result<Vec<CardDelta>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, from_repo, to_repo, text, context, priority, status, note, \
             labels, parent_card_id, source_url, source_type, sort_order, \
             created_at, updated_at, deleted_at, assigned_at, started_at, completed_at, \
             problem, solution, acceptance \
             FROM tasks \
             WHERE updated_at > ?1 OR deleted_at > ?1 \
             ORDER BY COALESCE(updated_at, deleted_at) ASC",
        )?;

        let rows = stmt.query_map([since], |row| {
            Ok(CardDelta {
                id: row.get(0)?,
                from_repo: row.get(1)?,
                to_repo: row.get(2)?,
                text: row.get(3)?,
                context: row.get(4)?,
                priority: row.get(5)?,
                status: row.get(6)?,
                note: row.get(7)?,
                labels: row.get(8)?,
                parent_card_id: row.get(9)?,
                source_url: row.get(10)?,
                source_type: row.get(11)?,
                sort_order: row.get(12)?,
                created_at: row.get(13)?,
                updated_at: row.get(14)?,
                deleted_at: row.get(15)?,
                assigned_at: row.get(16)?,
                started_at: row.get(17)?,
                completed_at: row.get(18)?,
                problem: row.get(19)?,
                solution: row.get(20)?,
                acceptance: row.get(21)?,
            })
        })?;

        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(LegionError::Database)
    }

    /// Get schedule deltas for multi-node sync.
    ///
    /// Returns all schedules that have been modified or soft-deleted since
    /// the given timestamp. Used for delta synchronization between nodes.
    #[allow(dead_code)] // Used by sync broadcast in #249
    pub fn get_schedule_deltas_since(&self, since: &str) -> Result<Vec<ScheduleDelta>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, name, cron, command, repo, enabled, last_run, next_run, \
             created_at, updated_at, deleted_at, active_start, active_end \
             FROM schedules \
             WHERE updated_at > ?1 OR deleted_at > ?1 \
             ORDER BY COALESCE(updated_at, deleted_at) ASC",
        )?;

        let rows = stmt.query_map([since], |row| {
            let enabled_int: i32 = row.get(5)?;
            Ok(ScheduleDelta {
                id: row.get(0)?,
                name: row.get(1)?,
                cron: row.get(2)?,
                command: row.get(3)?,
                repo: row.get(4)?,
                enabled: enabled_int != 0,
                last_run: row.get(6)?,
                next_run: row.get(7)?,
                created_at: row.get(8)?,
                updated_at: row.get(9)?,
                deleted_at: row.get(10)?,
                active_start: row.get(11)?,
                active_end: row.get(12)?,
            })
        })?;

        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(LegionError::Database)
    }

    /// Get uncertainty prediction deltas for multi-node sync.
    ///
    /// Returns all uncertainty_prediction rows modified or soft-deleted since
    /// the given timestamp. Predictions transition through emitted -> witnessed
    /// -> calibrated -> orphaned -> retired, so updated_at advances on every
    /// state change.
    #[allow(dead_code)] // Wired into sync broadcast once #358 hooks land.
    pub fn get_uncertainty_prediction_deltas_since(
        &self,
        since: &str,
    ) -> Result<Vec<UncertaintyPredictionDelta>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, surface, feature_key, input_fingerprint, model, model_version, \
             claimed_confidence, prediction_payload, state, outcome_label, outcome_payload, \
             outcome_correctness, cohort_key, created_at, updated_at, witnessed_at, \
             orphan_after, deleted_at \
             FROM uncertainty_prediction \
             WHERE updated_at > ?1 OR deleted_at > ?1 \
             ORDER BY COALESCE(updated_at, deleted_at) ASC",
        )?;

        let rows = stmt.query_map([since], |row| {
            Ok(UncertaintyPredictionDelta {
                id: row.get(0)?,
                surface: row.get(1)?,
                feature_key: row.get(2)?,
                input_fingerprint: row.get(3)?,
                model: row.get(4)?,
                model_version: row.get(5)?,
                claimed_confidence: row.get(6)?,
                prediction_payload: row.get(7)?,
                state: row.get(8)?,
                outcome_label: row.get(9)?,
                outcome_payload: row.get(10)?,
                outcome_correctness: row.get(11)?,
                cohort_key: row.get(12)?,
                created_at: row.get(13)?,
                updated_at: row.get(14)?,
                witnessed_at: row.get(15)?,
                orphan_after: row.get(16)?,
                deleted_at: row.get(17)?,
            })
        })?;

        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(LegionError::Database)
    }

    /// Get uncertainty calibration snapshot deltas for multi-node sync.
    ///
    /// Each node computes its own snapshots from synced predictions; sync
    /// propagates rows so peers can compare drift across the cluster without
    /// re-running the roller. LWW keyed by id with updated_at as tiebreaker.
    #[allow(dead_code)] // Wired into sync broadcast once #359 daemon roller lands.
    pub fn get_uncertainty_calibration_snapshot_deltas_since(
        &self,
        since: &str,
    ) -> Result<Vec<UncertaintyCalibrationSnapshotDelta>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, cohort_key, bucket_lower, bucket_upper, claimed_confidence, \
             actual_correctness, actual_correctness_raw, prediction_count, orphan_count, \
             brier_score, computed_at, updated_at, deleted_at \
             FROM uncertainty_calibration_snapshot \
             WHERE updated_at > ?1 OR deleted_at > ?1 \
             ORDER BY COALESCE(updated_at, deleted_at) ASC",
        )?;

        let rows = stmt.query_map([since], |row| {
            Ok(UncertaintyCalibrationSnapshotDelta {
                id: row.get(0)?,
                cohort_key: row.get(1)?,
                bucket_lower: row.get(2)?,
                bucket_upper: row.get(3)?,
                claimed_confidence: row.get(4)?,
                actual_correctness: row.get(5)?,
                actual_correctness_raw: row.get(6)?,
                prediction_count: row.get(7)?,
                orphan_count: row.get(8)?,
                brier_score: row.get(9)?,
                computed_at: row.get(10)?,
                updated_at: row.get(11)?,
                deleted_at: row.get(12)?,
            })
        })?;

        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(LegionError::Database)
    }

    /// Hard-delete tombstones older than the given number of days.
    ///
    /// Removes soft-deleted rows (where deleted_at IS NOT NULL) that are older
    /// than `retention_days`. Returns a struct with counts of deleted rows per table.
    ///
    /// This is the housekeeper cleanup for multi-node sync. Once tombstones have
    /// propagated to all nodes (typically within hours), they can be permanently
    /// removed to reclaim space. A 30-day retention is recommended.
    pub fn cleanup_tombstones(&self, retention_days: i64) -> Result<TombstoneCleanupResult> {
        let cutoff = (Utc::now() - chrono::Duration::days(retention_days)).to_rfc3339();

        let reflections = self.conn.execute(
            "DELETE FROM reflections WHERE deleted_at IS NOT NULL AND deleted_at < ?1",
            [&cutoff],
        )? as u64;

        let tasks = self.conn.execute(
            "DELETE FROM tasks WHERE deleted_at IS NOT NULL AND deleted_at < ?1",
            [&cutoff],
        )? as u64;

        let schedules = self.conn.execute(
            "DELETE FROM schedules WHERE deleted_at IS NOT NULL AND deleted_at < ?1",
            [&cutoff],
        )? as u64;

        let uncertainty_predictions = self.conn.execute(
            "DELETE FROM uncertainty_prediction WHERE deleted_at IS NOT NULL AND deleted_at < ?1",
            [&cutoff],
        )? as u64;

        let uncertainty_calibration_snapshots = self.conn.execute(
            "DELETE FROM uncertainty_calibration_snapshot \
             WHERE deleted_at IS NOT NULL AND deleted_at < ?1",
            [&cutoff],
        )? as u64;

        Ok(TombstoneCleanupResult {
            reflections,
            tasks,
            schedules,
            uncertainty_predictions,
            uncertainty_calibration_snapshots,
        })
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
             last_recalled_at, parent_id FROM reflections WHERE id IN ({}) AND deleted_at IS NULL",
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
            "SELECT id, repo, text, created_at, updated_at, audience, domain, tags, recall_count, last_recalled_at, parent_id FROM reflections WHERE repo = ?1 AND deleted_at IS NULL ORDER BY created_at DESC",
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
            "SELECT id, repo, text, created_at, updated_at, audience, domain, tags, recall_count, last_recalled_at, parent_id FROM reflections WHERE repo = ?1 AND audience = 'self' AND deleted_at IS NULL ORDER BY created_at DESC LIMIT ?2",
        )?;

        let rows = stmt.query_map(rusqlite::params![repo, limit], map_reflection_row)?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(LegionError::Database)
    }

    /// Retrieve latest reflections matching a specific domain for a repository.
    ///
    /// Bypasses search entirely -- pure SQL lookup by domain. Used for
    /// reserved domains like `identity` and `snooze` that get injected
    /// on every session start without needing a search query.
    pub fn get_reflections_by_domain(
        &self,
        repo: &str,
        domain: &str,
        limit: usize,
    ) -> Result<Vec<Reflection>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, repo, text, created_at, updated_at, audience, domain, tags, recall_count, last_recalled_at, parent_id \
             FROM reflections WHERE repo = ?1 AND domain = ?2 AND deleted_at IS NULL ORDER BY created_at DESC LIMIT ?3",
        )?;

        let rows = stmt.query_map(rusqlite::params![repo, domain, limit], map_reflection_row)?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(LegionError::Database)
    }

    /// Retrieve identity reflections that are chain roots or orphans for
    /// `whoami`. Excludes reflections with a `parent_id` (chain children),
    /// because their content is reachable via `legion chain --id <root>`
    /// and including them would bloat the whoami banner past the inline
    /// context budget. Ordered newest first.
    /// Fetch the root reflections (no parent) for a repo in a given domain,
    /// newest first. Backs the SessionStart boot banners: `whoami`
    /// (domain=identity, who I am) and `whatami` (domain=workflow, how I operate).
    pub fn get_domain_roots(
        &self,
        repo: &str,
        domain: &str,
        limit: usize,
    ) -> Result<Vec<Reflection>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, repo, text, created_at, updated_at, audience, domain, tags, recall_count, last_recalled_at, parent_id \
             FROM reflections WHERE repo = ?1 AND domain = ?2 AND parent_id IS NULL AND deleted_at IS NULL ORDER BY created_at DESC LIMIT ?3",
        )?;

        let rows = stmt.query_map(rusqlite::params![repo, domain, limit], map_reflection_row)?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(LegionError::Database)
    }

    /// Identity roots (domain=identity). Thin wrapper over [`get_domain_roots`].
    pub fn get_identity_roots(&self, repo: &str, limit: usize) -> Result<Vec<Reflection>> {
        self.get_domain_roots(repo, "identity", limit)
    }

    /// Retrieve active (non-archived) bullpen posts, ordered newest first.
    pub fn get_board_posts(&self) -> Result<Vec<Reflection>> {
        self.get_board_posts_filtered(false)
    }

    /// Like `get_board_posts` but with opt-in switches for past-TTL,
    /// non-evergreen posts (#376) and resolved threads (#362). Used by
    /// `legion bullpen --include-stale` / `--include-resolved` for operator
    /// review. Agents must never reach this with either set to `true`.
    pub fn get_board_posts_filtered(&self, include_stale: bool) -> Result<Vec<Reflection>> {
        self.get_board_posts_filtered_full(include_stale, false)
    }

    /// Full filter: independently opt into stale and resolved rows.
    pub fn get_board_posts_filtered_full(
        &self,
        include_stale: bool,
        include_resolved: bool,
    ) -> Result<Vec<Reflection>> {
        let now = Utc::now().to_rfc3339();
        let decay_clause = if include_stale {
            ""
        } else {
            " AND (evergreen = 1 OR expires_at > ?1)"
        };
        let resolved_clause = if include_resolved {
            ""
        } else {
            " AND resolved_at IS NULL"
        };
        let sql = format!(
            "SELECT id, repo, text, created_at, updated_at, audience, domain, tags, recall_count, last_recalled_at, parent_id \
             FROM reflections WHERE audience = 'team' AND archived_at IS NULL AND deleted_at IS NULL{decay_clause}{resolved_clause} ORDER BY created_at DESC"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = if include_stale {
            stmt.query_map([], map_reflection_row)?
                .collect::<std::result::Result<Vec<_>, _>>()
        } else {
            stmt.query_map([&now], map_reflection_row)?
                .collect::<std::result::Result<Vec<_>, _>>()
        };
        rows.map_err(LegionError::Database)
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
        //
        // Decay filter (#376): the channel notification path is the most
        // expensive token-cost surface for stale posts, so the filter applies
        // here too. Backdated inserts past TTL are silently skipped instead of
        // pushed.
        let now = Utc::now().to_rfc3339();
        let mut stmt = self.conn.prepare(
            "SELECT id, repo, text, created_at, updated_at, audience, domain, tags, recall_count, last_recalled_at, parent_id \
             FROM reflections \
             WHERE audience = 'team' AND archived_at IS NULL AND deleted_at IS NULL \
               AND (evergreen = 1 OR expires_at > ?4) \
               AND resolved_at IS NULL \
               AND (created_at > ?1 OR (created_at = ?1 AND id > ?2)) \
             ORDER BY created_at ASC, id ASC \
             LIMIT ?3",
        )?;

        let rows = stmt.query_map(
            rusqlite::params![since_created_at, since_id, limit as i64, now],
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
             WHERE audience = 'team' AND archived_at IS NULL AND deleted_at IS NULL \
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
            "SELECT id, repo, text, created_at, updated_at, audience, domain, tags, recall_count, last_recalled_at, parent_id \
             FROM reflections \
             WHERE audience = 'team' AND archived_at IS NULL AND deleted_at IS NULL \
             AND (evergreen = 1 OR expires_at > ?2) \
             AND resolved_at IS NULL \
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
            "SELECT id, repo, text, created_at, updated_at, audience, domain, tags, recall_count, last_recalled_at, parent_id \
             FROM reflections WHERE audience = 'team' AND archived_at IS NOT NULL AND deleted_at IS NULL ORDER BY created_at DESC",
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
            "UPDATE reflections SET archived_at = ?1, updated_at = ?1 \
             WHERE audience = 'team' AND archived_at IS NULL AND deleted_at IS NULL \
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
        let now = Utc::now().to_rfc3339();
        let mut stmt = self.conn.prepare(
            "SELECT COUNT(*) FROM reflections WHERE audience = 'team' \
             AND archived_at IS NULL AND deleted_at IS NULL \
             AND (evergreen = 1 OR expires_at > ?2) \
             AND resolved_at IS NULL \
             AND created_at > COALESCE( \
                 (SELECT last_read_at FROM board_reads WHERE reader_repo = ?1), \
                 '' \
             )",
        )?;

        let count: u64 = stmt
            .query_row(rusqlite::params![reader_repo, &now], |row| row.get(0))
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

    /// Read the channel-delivery cursor for `reader_repo`.
    ///
    /// Used by the MCP notifier at cold boot so a session that was offline
    /// while a directed signal was filed picks it up on the first poll tick
    /// after it comes back online. Returns `(last_read_at, last_read_id)`,
    /// matching what `advance_board_read_cursor` writes. `last_read_id` is
    /// an empty string for rows written before the #400 migration.
    pub fn get_board_read_cursor(&self, reader_repo: &str) -> Result<Option<(String, String)>> {
        let mut stmt = self
            .conn
            .prepare("SELECT last_read_at, last_read_id FROM board_reads WHERE reader_repo = ?1")?;
        stmt.query_row([reader_repo], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .optional()
        .map_err(LegionError::Database)
    }

    /// Advance the channel-delivery cursor for `reader_repo` to
    /// `(last_read_at, last_read_id)`.
    ///
    /// Forward-only: the upsert refuses to move the cursor backwards. The
    /// composite ordering is `(last_read_at, last_read_id)`, which mirrors
    /// the strict-`>` comparator used by `get_board_posts_since` so a row
    /// recorded here is excluded from re-emission on the next poll.
    /// Called by the MCP notifier after each successful delivery.
    ///
    /// `last_read_at` MUST share the offset shape every other legion write
    /// path uses -- `chrono::Utc::now().to_rfc3339()`, which renders a
    /// `+00:00` suffix (NOT `Z`). Lexicographic SQL `>` is correct only when
    /// every timestamp it compares is rendered with the same fixed offset
    /// string; mixing `+00:00` with `Z` (or any other offset) would
    /// mis-order rows that compare correctly chronologically. Every caller
    /// in-tree obeys this convention; the contract is documented here so
    /// future callers do not need to rediscover the constraint.
    pub fn advance_board_read_cursor(
        &self,
        reader_repo: &str,
        last_read_at: &str,
        last_read_id: &str,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT INTO board_reads (reader_repo, last_read_at, last_read_id) \
             VALUES (?1, ?2, ?3) \
             ON CONFLICT(reader_repo) DO UPDATE SET \
                 last_read_at = excluded.last_read_at, \
                 last_read_id = excluded.last_read_id \
             WHERE excluded.last_read_at > board_reads.last_read_at \
                OR (excluded.last_read_at = board_reads.last_read_at \
                    AND excluded.last_read_id > board_reads.last_read_id)",
            (reader_repo, last_read_at, last_read_id),
        )?;
        Ok(())
    }

    /// Find unhandled signals directed at a specific repo.
    ///
    /// `names` is the full addressable name set for this repo: the repo's
    /// `recipient()` value plus any `broadcast_tags`. A signal is returned
    /// when its text starts with (or contains mid-string) `@<name>` for any
    /// name in `names`, OR when it starts with `@all` / `@everyone`.
    ///
    /// Uses the `watch_handled` table for per-repo dedup keyed on `repo_name`
    /// (not on recipient or tags), so `@all` / `@everyone` broadcasts wake
    /// each repo exactly once, and a tag-addressed signal wakes each tagged
    /// repo exactly once. `repo_name` is also the self-signal exclusion key.
    ///
    /// When `names` is empty the function returns an empty vec (nothing to
    /// match). When `names` has one entry it matches only that name plus the
    /// reserved broadcasts, preserving backward-compatible behavior.
    pub fn get_unhandled_signals_for_repo(
        &self,
        repo_name: &str,
        names: &[String],
        since: Option<&str>,
    ) -> Result<Vec<Reflection>> {
        if names.is_empty() {
            return Ok(Vec::new());
        }

        let now = Utc::now().to_rfc3339();

        // Build LIKE patterns for every addressable name plus the reserved
        // broadcast aliases. Parameters are bound positionally, so we
        // pre-compute the full set before building the SQL.
        //
        // Reserved pattern slots (always present, param-indexed from 1):
        //   1: "@all %"
        //   2: "%@all %"
        //   3: "@everyone %"
        //   4: "%@everyone %"
        //
        // Per-name slots (two per name, starting at param 5):
        //   5k-4: "@<name> %"
        //   5k-3: "%@<name> %"
        //   ...
        //
        // Fixed trailing params:
        //   repo_name (for watch_handled join + self-exclusion)
        //   now       (for expires_at > now)
        //   since_ts  (optional, last)

        // Build the dynamic name OR clauses. Each name contributes two LIKE
        // patterns: one anchored at the start ("@name ...") and one mid-text
        // ("%@name ..."). The param indices start at 5 (1-4 are the reserved
        // @all / @everyone patterns).
        let mut name_clauses: Vec<String> = Vec::with_capacity(names.len() * 2);
        for (i, _) in names.iter().enumerate() {
            // Each name occupies two consecutive parameter slots: 2*(i+2)+1 and 2*(i+2)+2.
            // With 4 reserved slots (params 1-4), name[0] uses params 5,6;
            // name[1] uses 7,8; etc.
            let p_start = 5 + i * 2;
            let p_mid = 6 + i * 2;
            name_clauses.push(format!(
                "r.text LIKE ?{} OR r.text LIKE ?{}",
                p_start, p_mid
            ));
        }

        // The last two fixed params follow all name params.
        let n_name_params = names.len() * 2;
        let p_repo = 5 + n_name_params; // repo_name
        let p_now = 6 + n_name_params; // now (expires_at check)
        let p_since = 7 + n_name_params; // since_ts (optional)

        let since_clause = if since.is_some() {
            format!(" AND r.created_at > ?{}", p_since)
        } else {
            String::new()
        };

        // Assemble the WHERE text fragment: reserved broadcasts + per-name.
        let text_filter = {
            let mut parts: Vec<String> = vec![
                "r.text LIKE ?1".to_string(),
                "r.text LIKE ?2".to_string(),
                "r.text LIKE ?3".to_string(),
                "r.text LIKE ?4".to_string(),
            ];
            parts.extend(name_clauses);
            parts.join(" OR ")
        };

        let query = format!(
            "SELECT r.id, r.repo, r.text, r.created_at, r.updated_at, r.audience, r.domain, r.tags, \
             r.recall_count, r.last_recalled_at, r.parent_id \
             FROM reflections r \
             LEFT JOIN watch_handled wh ON wh.signal_id = r.id AND wh.repo_name = ?{repo_param} \
             WHERE r.audience = 'team' AND r.deleted_at IS NULL \
               AND (r.evergreen = 1 OR r.expires_at > ?{now_param}) \
               AND r.resolved_at IS NULL \
               AND wh.signal_id IS NULL \
               AND ({text_filter}) \
               AND r.repo != ?{repo_param}{since_clause} \
             ORDER BY r.created_at ASC",
            repo_param = p_repo,
            now_param = p_now,
            text_filter = text_filter,
            since_clause = since_clause,
        );

        let mut stmt = self.conn.prepare(&query)?;

        // Bind params in slot order: reserved broadcasts, then per-name pairs,
        // then repo_name, now, and optionally since_ts.
        use rusqlite::types::ToSql;
        // Slots 1-4: reserved broadcasts. Per-name slots follow (two per name),
        // then repo_name and now as fixed trailing params.
        let mut params: Vec<Box<dyn ToSql>> = vec![
            Box::new("@all %".to_string()),
            Box::new("%@all %".to_string()),
            Box::new("@everyone %".to_string()),
            Box::new("%@everyone %".to_string()),
        ];
        for name in names {
            params.push(Box::new(format!("@{} %", name)));
            params.push(Box::new(format!("%@{} %", name)));
        }
        params.push(Box::new(repo_name.to_string()));
        params.push(Box::new(now));

        // Optional since_ts.
        let since_owned: Option<String> = since.map(str::to_string);
        if let Some(ref ts) = since_owned {
            params.push(Box::new(ts.clone()));
        }

        let param_refs: Vec<&dyn ToSql> = params.iter().map(|p| p.as_ref()).collect();
        let rows = stmt.query_map(param_refs.as_slice(), map_reflection_row)?;
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
            .prepare("SELECT id, repo, text, created_at, updated_at, audience, domain, tags, recall_count, last_recalled_at, parent_id FROM reflections WHERE deleted_at IS NULL")?;
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
                     MAX(created_at) as newest FROM reflections WHERE deleted_at IS NULL";

        let sql = match repo {
            Some(_) => format!("{base} AND repo = ?1 GROUP BY repo"),
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
        let now = Utc::now();
        let cutoff = (now - chrono::Duration::hours(hours)).to_rfc3339();
        let now_str = now.to_rfc3339();
        let mut stmt = self.conn.prepare(
            "SELECT id, repo, text, created_at, updated_at, audience, domain, tags, recall_count, last_recalled_at, parent_id \
             FROM reflections WHERE audience = 'team' AND archived_at IS NULL AND deleted_at IS NULL \
             AND (evergreen = 1 OR expires_at > ?2) \
             AND resolved_at IS NULL \
             AND created_at > ?1 ORDER BY created_at DESC",
        )?;
        let rows = stmt.query_map([&cutoff, &now_str], map_reflection_row)?;
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
            "SELECT id, repo, text, created_at, updated_at, audience, domain, tags, recall_count, last_recalled_at, parent_id \
             FROM reflections WHERE repo != ?1 AND recall_count > 0 AND deleted_at IS NULL ORDER BY recall_count DESC LIMIT ?2",
        )?;
        let rows = stmt.query_map(rusqlite::params![exclude_repo, limit], map_reflection_row)?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(LegionError::Database)
    }

    /// Get all distinct repo names from reflections.
    pub fn get_distinct_repos(&self) -> Result<Vec<String>> {
        let mut stmt = self.conn.prepare(
            "SELECT DISTINCT repo FROM reflections WHERE deleted_at IS NULL ORDER BY repo",
        )?;
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
             FROM reflections WHERE deleted_at IS NULL GROUP BY repo ORDER BY repo",
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

    /// Get the most recent created_at timestamp from reflections.
    pub fn get_max_created_at(&self) -> Result<Option<String>> {
        let mut stmt = self
            .conn
            .prepare("SELECT MAX(created_at) FROM reflections WHERE deleted_at IS NULL")?;
        let result: Option<String> = stmt
            .query_row([], |row| row.get(0))
            .map_err(LegionError::Database)?;
        Ok(result)
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
            "INSERT INTO schedules (id, name, cron, command, repo, enabled, next_run, created_at, updated_at, active_start, active_end) \
             VALUES (?1, ?2, ?3, ?4, ?5, 1, ?6, ?7, ?8, ?9, ?10)",
            rusqlite::params![&id, name, cron, command, repo, &next_run_str, &created_at, &created_at, active_start, active_end],
        )?;

        Ok(id)
    }

    /// Get all schedules that are enabled, due (next_run <= now), and within
    /// their active time window (if set).
    pub fn get_due_schedules(&self) -> Result<Vec<Schedule>> {
        let now = Utc::now();
        let now_str = now.to_rfc3339();
        let mut stmt = self.conn.prepare(
            "SELECT id, name, cron, command, repo, enabled, last_run, next_run, created_at, updated_at, active_start, active_end \
             FROM schedules WHERE enabled = 1 AND next_run <= ?1 AND deleted_at IS NULL",
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
            .query_row(
                "SELECT cron FROM schedules WHERE id = ?1 AND deleted_at IS NULL",
                [id],
                |row| row.get(0),
            )
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
            "UPDATE schedules SET last_run = ?1, next_run = ?2, updated_at = ?1 WHERE id = ?3 AND deleted_at IS NULL",
            rusqlite::params![&now_str, &next_run_str, id],
        )?;

        Ok(())
    }

    /// List all schedules.
    pub fn list_schedules(&self) -> Result<Vec<Schedule>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, name, cron, command, repo, enabled, last_run, next_run, created_at, updated_at, active_start, active_end \
             FROM schedules WHERE deleted_at IS NULL ORDER BY created_at",
        )?;
        let rows = stmt.query_map([], map_schedule_row)?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(LegionError::Database)
    }

    /// Toggle a schedule's enabled state. Returns false if schedule not found.
    pub fn toggle_schedule(&self, id: &str, enabled: bool) -> Result<bool> {
        let enabled_int: i32 = if enabled { 1 } else { 0 };
        let now = Utc::now().to_rfc3339();
        let rows = self.conn.execute(
            "UPDATE schedules SET enabled = ?1, updated_at = ?3 WHERE id = ?2 AND deleted_at IS NULL",
            rusqlite::params![enabled_int, id, &now],
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

    /// Soft-delete a schedule by setting its deleted_at timestamp.
    ///
    /// Unlike `delete_schedule` (hard delete), this preserves the row for
    /// multi-node sync tombstone propagation. The row becomes invisible to
    /// normal queries but can still be synced to other nodes.
    #[allow(dead_code)] // Used by sync module in #248
    pub fn soft_delete_schedule(&self, id: &str) -> Result<bool> {
        let now = chrono::Utc::now().to_rfc3339();
        let rows = self.conn.execute(
            "UPDATE schedules SET deleted_at = ?1, updated_at = ?1 WHERE id = ?2 AND deleted_at IS NULL",
            rusqlite::params![now, id],
        )?;
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

        // Always update updated_at for LWW conflict resolution
        let now = Utc::now().to_rfc3339();
        updates.push(format!("updated_at = ?{}", params.len() + 1));
        params.push(now);

        let query = format!(
            "UPDATE schedules SET {} WHERE id = ?{} AND deleted_at IS NULL",
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
            "SELECT id, repo, text, created_at, updated_at, audience, domain, tags, recall_count, last_recalled_at, parent_id \
             FROM reflections WHERE parent_id IS NOT NULL AND deleted_at IS NULL AND created_at > ?1 ORDER BY created_at DESC",
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

    // -- Statusline Samples ------------------------------------------------------

    /// Insert a rate-limit sample captured from a Claude Code statusline render.
    ///
    /// Note the VALUES clause reuses bind index `?4` for both `sampled_at`
    /// and `updated_at`. This is intentional on INSERT -- a fresh row's
    /// updated_at equals its sampled_at -- but a future UPDATE path must
    /// re-bind updated_at to a fresh timestamp. Don't copy the pattern
    /// blindly into an UPDATE statement.
    pub fn insert_rate_limit_sample(
        &self,
        sample: &crate::statusline::RateLimitSample,
    ) -> Result<String> {
        self.conn.execute(
            "INSERT INTO rate_limit_samples (id, hostname, session_id, sampled_at, \
             five_hour_pct, five_hour_resets_at, seven_day_pct, seven_day_resets_at, \
             model, updated_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?4)",
            rusqlite::params![
                sample.id,
                sample.hostname,
                sample.session_id,
                sample.sampled_at,
                sample.five_hour_pct,
                sample.five_hour_resets_at,
                sample.seven_day_pct,
                sample.seven_day_resets_at,
                sample.model,
            ],
        )?;
        Ok(sample.id.clone())
    }

    /// Insert a usage sample captured from a Claude Code statusline render.
    ///
    /// VALUES reuses bind index `?12` for both `sampled_at` and
    /// `updated_at`. Intentional on INSERT; a future UPDATE path must
    /// re-bind updated_at separately. See `insert_rate_limit_sample`
    /// for the same pattern.
    pub fn insert_usage_sample(&self, sample: &crate::statusline::UsageSample) -> Result<String> {
        self.conn.execute(
            "INSERT INTO usage_samples (id, hostname, session_id, turn_index, model, \
             input_tokens, output_tokens, cache_write_tokens, cache_read_tokens, \
             effective_tokens, error_bytes, sampled_at, updated_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?12)",
            rusqlite::params![
                sample.id,
                sample.hostname,
                sample.session_id,
                sample.turn_index,
                sample.model,
                sample.input_tokens,
                sample.output_tokens,
                sample.cache_write_tokens,
                sample.cache_read_tokens,
                sample.effective_tokens,
                sample.error_bytes,
                sample.sampled_at,
            ],
        )?;
        Ok(sample.id.clone())
    }

    /// Most recent rate-limit sample across all hosts in the cluster.
    /// Returns `None` when no sample has been written yet. Used by the
    /// budget gate to read cluster-wide headroom.
    #[allow(dead_code)] // Consumed by the upcoming `legion budget` subcommand.
    pub fn latest_rate_limit_sample(&self) -> Result<Option<crate::statusline::RateLimitSample>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, hostname, session_id, sampled_at, five_hour_pct, \
             five_hour_resets_at, seven_day_pct, seven_day_resets_at, model \
             FROM rate_limit_samples WHERE deleted_at IS NULL \
             ORDER BY sampled_at DESC LIMIT 1",
        )?;
        let mut rows = stmt.query([])?;
        match rows.next().map_err(LegionError::Database)? {
            Some(row) => Ok(Some(map_rate_limit_sample_row(row)?)),
            None => Ok(None),
        }
    }

    /// Most recent rate-limit sample for a single host. Used by the watch
    /// quota-panic gate, which only cares about THIS node's headroom --
    /// a peer node hitting its cap should not gate this node's spawns.
    pub fn latest_rate_limit_sample_for_host(
        &self,
        hostname: &str,
    ) -> Result<Option<crate::statusline::RateLimitSample>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, hostname, session_id, sampled_at, five_hour_pct, \
             five_hour_resets_at, seven_day_pct, seven_day_resets_at, model \
             FROM rate_limit_samples WHERE deleted_at IS NULL AND hostname = ?1 \
             ORDER BY sampled_at DESC LIMIT 1",
        )?;
        let mut rows = stmt.query(rusqlite::params![hostname])?;
        match rows.next().map_err(LegionError::Database)? {
            Some(row) => Ok(Some(map_rate_limit_sample_row(row)?)),
            None => Ok(None),
        }
    }

    /// Most recent rate-limit sample per hostname.
    ///
    /// Returns one row per host, picking the newest `sampled_at` for each.
    /// Soft-deleted rows are skipped. Cluster-sync pushes samples from
    /// peers into the same table, so a single node running this query
    /// sees the whole mesh once sync has settled.
    ///
    /// Used by `legion mesh headroom / pick` to rank hosts by available
    /// capacity. Paired with [`latest_usage_samples_per_host`].
    pub fn latest_rate_limit_samples_per_host(
        &self,
    ) -> Result<Vec<crate::statusline::RateLimitSample>> {
        // ROW_NUMBER partitioned by hostname gives a deterministic tie-break
        // when two rows share the same MAX(sampled_at). Without it, the
        // IN-subquery variant returned both rows and the caller's BTreeMap
        // collapsed them in insertion order, so a pair of statusline writes
        // within one RFC3339-second could produce non-deterministic scores.
        // Tie-break: newer sampled_at, then higher id (UUIDv7 embeds time).
        let mut stmt = self.conn.prepare(
            "SELECT id, hostname, session_id, sampled_at, five_hour_pct, \
             five_hour_resets_at, seven_day_pct, seven_day_resets_at, model \
             FROM ( \
                 SELECT id, hostname, session_id, sampled_at, five_hour_pct, \
                     five_hour_resets_at, seven_day_pct, seven_day_resets_at, model, \
                     ROW_NUMBER() OVER ( \
                         PARTITION BY hostname \
                         ORDER BY sampled_at DESC, id DESC \
                     ) AS rn \
                 FROM rate_limit_samples \
                 WHERE deleted_at IS NULL \
             ) \
             WHERE rn = 1 \
             ORDER BY hostname",
        )?;
        let rows = stmt.query_map([], map_rate_limit_sample_row)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// Most recent usage sample per hostname. Pair of
    /// [`latest_rate_limit_samples_per_host`].
    pub fn latest_usage_samples_per_host(&self) -> Result<Vec<crate::statusline::UsageSample>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, hostname, session_id, turn_index, model, \
             input_tokens, output_tokens, cache_write_tokens, cache_read_tokens, \
             effective_tokens, error_bytes, sampled_at \
             FROM ( \
                 SELECT id, hostname, session_id, turn_index, model, \
                     input_tokens, output_tokens, cache_write_tokens, cache_read_tokens, \
                     effective_tokens, error_bytes, sampled_at, \
                     ROW_NUMBER() OVER ( \
                         PARTITION BY hostname \
                         ORDER BY sampled_at DESC, id DESC \
                     ) AS rn \
                 FROM usage_samples \
                 WHERE deleted_at IS NULL \
             ) \
             WHERE rn = 1 \
             ORDER BY hostname",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(crate::statusline::UsageSample {
                id: row.get(0)?,
                hostname: row.get(1)?,
                session_id: row.get(2)?,
                turn_index: row.get(3)?,
                model: row.get(4)?,
                input_tokens: row.get(5)?,
                output_tokens: row.get(6)?,
                cache_write_tokens: row.get(7)?,
                cache_read_tokens: row.get(8)?,
                effective_tokens: row.get(9)?,
                error_bytes: row.get(10)?,
                sampled_at: row.get(11)?,
            })
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
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

/// A persona wake lease -- "host H is handling signal S for persona P until T".
///
/// Acquired by watch before spawning an agent in response to a wake signal.
/// Other watchers (on this node or peers) see the live lease and skip their
/// own spawn. Heartbeats keep `expires_at` rolling forward; a crashed session
/// whose heartbeats stop lets the lease age out via TTL.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq)]
pub struct PersonaWakeLease {
    pub persona_id: String,
    pub signal_id: String,
    pub acquired_by_host: String,
    pub acquired_at: String,
    pub heartbeat_at: String,
    pub expires_at: String,
    pub updated_at: String,
    pub deleted_at: Option<String>,
}

fn map_persona_lease_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<PersonaWakeLease> {
    Ok(PersonaWakeLease {
        persona_id: row.get(0)?,
        signal_id: row.get(1)?,
        acquired_by_host: row.get(2)?,
        acquired_at: row.get(3)?,
        heartbeat_at: row.get(4)?,
        expires_at: row.get(5)?,
        deleted_at: row.get(6)?,
        updated_at: row.get(7)?,
    })
}

impl Database {
    /// Try to acquire a persona wake lease. Returns `Ok(true)` on success,
    /// `Ok(false)` when a live lease for `(persona_id, signal_id)` is already
    /// held. Expired or soft-deleted leases are treated as free and may be
    /// claimed by the caller.
    ///
    /// Atomicity: a single `UPDATE ... WHERE expires_at <= now OR deleted_at IS NOT NULL`
    /// followed by `INSERT OR IGNORE` runs inside a transaction. Both
    /// statements take SQLite's write lock so cross-process races are
    /// serialized by the DB file lock; the caller sees the outcome via
    /// `rows_changed()`. This matches the issue spec: "INSERT OR FAIL with
    /// primary-key collision; first-writer-wins."
    pub fn try_acquire_persona_lease(
        &self,
        persona_id: &str,
        signal_id: &str,
        host: &str,
        lease_ttl: std::time::Duration,
    ) -> Result<bool> {
        let now = Utc::now();
        let now_str = now.to_rfc3339();
        let expires = (now
            + chrono::Duration::from_std(lease_ttl)
                .unwrap_or_else(|_| chrono::Duration::minutes(10)))
        .to_rfc3339();

        let tx = self.conn.unchecked_transaction()?;

        // Reclaim stale rows so INSERT OR IGNORE below can succeed against
        // them. Scoped by PK so this only touches the row we are trying to
        // acquire -- no broad sweep here.
        tx.execute(
            "UPDATE persona_wake_leases \
             SET acquired_by_host = ?1, acquired_at = ?2, heartbeat_at = ?2, \
                 expires_at = ?3, updated_at = ?2, deleted_at = NULL \
             WHERE persona_id = ?4 AND signal_id = ?5 \
               AND (deleted_at IS NOT NULL OR expires_at <= ?2)",
            rusqlite::params![host, &now_str, &expires, persona_id, signal_id],
        )?;

        let inserted = tx.execute(
            "INSERT OR IGNORE INTO persona_wake_leases \
             (persona_id, signal_id, acquired_by_host, acquired_at, heartbeat_at, \
              expires_at, updated_at, deleted_at) \
             VALUES (?1, ?2, ?3, ?4, ?4, ?5, ?4, NULL)",
            rusqlite::params![persona_id, signal_id, host, &now_str, &expires],
        )?;

        // If INSERT OR IGNORE inserted (1) or the reclaim UPDATE touched a
        // stale row we now own, the lease is ours. Confirm we hold it by
        // reading back -- covers the edge case where the stale-reclaim
        // UPDATE succeeded but the INSERT was a no-op.
        let holder: Option<String> = tx
            .query_row(
                "SELECT acquired_by_host FROM persona_wake_leases \
                 WHERE persona_id = ?1 AND signal_id = ?2 AND deleted_at IS NULL",
                rusqlite::params![persona_id, signal_id],
                |r| r.get(0),
            )
            .optional()?;

        tx.commit()?;
        let _ = inserted;
        Ok(holder.as_deref() == Some(host))
    }

    /// Refresh every live lease held by `host`, extending `expires_at` to
    /// `now + ttl`. Returns the number of leases touched.
    pub fn heartbeat_persona_leases(&self, host: &str, ttl: std::time::Duration) -> Result<u64> {
        let now = Utc::now();
        let now_str = now.to_rfc3339();
        let expires = (now
            + chrono::Duration::from_std(ttl).unwrap_or_else(|_| chrono::Duration::minutes(10)))
        .to_rfc3339();

        let updated = self.conn.execute(
            "UPDATE persona_wake_leases \
             SET heartbeat_at = ?1, expires_at = ?2, updated_at = ?1 \
             WHERE acquired_by_host = ?3 AND deleted_at IS NULL",
            rusqlite::params![&now_str, &expires, host],
        )?;
        Ok(updated as u64)
    }

    /// Soft-delete one lease by (persona_id, signal_id). Returns true if a
    /// matching live lease existed. Idempotent on an already-released lease.
    ///
    /// Unscoped by host -- used by the operator CLI to forcibly drop any
    /// stuck lease. The watch reaper uses `release_persona_lease_if_owner`
    /// instead so a late-loser whose lease was overwritten by sync cannot
    /// accidentally release the winner's row.
    pub fn release_persona_lease(&self, persona_id: &str, signal_id: &str) -> Result<bool> {
        let now = Utc::now().to_rfc3339();
        let updated = self.conn.execute(
            "UPDATE persona_wake_leases \
             SET deleted_at = ?1, updated_at = ?1 \
             WHERE persona_id = ?2 AND signal_id = ?3 AND deleted_at IS NULL",
            rusqlite::params![&now, persona_id, signal_id],
        )?;
        Ok(updated > 0)
    }

    /// Like `release_persona_lease`, but only if the lease is still held by
    /// `host`. Used by the watch reaper so a late-loser whose lease was
    /// overwritten by a sync-resolved peer cannot release the peer's row.
    /// Returns true only when this host's row was released.
    pub fn release_persona_lease_if_owner(
        &self,
        persona_id: &str,
        signal_id: &str,
        host: &str,
    ) -> Result<bool> {
        let now = Utc::now().to_rfc3339();
        let updated = self.conn.execute(
            "UPDATE persona_wake_leases \
             SET deleted_at = ?1, updated_at = ?1 \
             WHERE persona_id = ?2 AND signal_id = ?3 \
               AND acquired_by_host = ?4 AND deleted_at IS NULL",
            rusqlite::params![&now, persona_id, signal_id, host],
        )?;
        Ok(updated > 0)
    }

    /// Soft-delete every live lease held by `host`. Called on daemon shutdown
    /// so a graceful exit does not leave ghost leases that must age out via TTL.
    #[allow(dead_code)] // wired by a future SIGTERM handler; kept in the API surface now
    pub fn release_persona_leases_by_host(&self, host: &str) -> Result<u64> {
        let now = Utc::now().to_rfc3339();
        let updated = self.conn.execute(
            "UPDATE persona_wake_leases \
             SET deleted_at = ?1, updated_at = ?1 \
             WHERE acquired_by_host = ?2 AND deleted_at IS NULL",
            rusqlite::params![&now, host],
        )?;
        Ok(updated as u64)
    }

    /// Return every live (non-expired, non-deleted) lease, optionally filtered
    /// to a single persona. Ordered oldest-first by `acquired_at` so the CLI
    /// lists leases in the order they were taken.
    pub fn list_persona_leases(&self, persona: Option<&str>) -> Result<Vec<PersonaWakeLease>> {
        let now = Utc::now().to_rfc3339();
        match persona {
            Some(p) => {
                let mut stmt = self.conn.prepare(
                    "SELECT persona_id, signal_id, acquired_by_host, acquired_at, \
                            heartbeat_at, expires_at, deleted_at, updated_at \
                     FROM persona_wake_leases \
                     WHERE deleted_at IS NULL AND expires_at > ?1 AND persona_id = ?2 \
                     ORDER BY acquired_at ASC",
                )?;
                Ok(stmt
                    .query_map(rusqlite::params![&now, p], map_persona_lease_row)?
                    .collect::<rusqlite::Result<Vec<_>>>()?)
            }
            None => {
                let mut stmt = self.conn.prepare(
                    "SELECT persona_id, signal_id, acquired_by_host, acquired_at, \
                            heartbeat_at, expires_at, deleted_at, updated_at \
                     FROM persona_wake_leases \
                     WHERE deleted_at IS NULL AND expires_at > ?1 \
                     ORDER BY acquired_at ASC",
                )?;
                Ok(stmt
                    .query_map(rusqlite::params![&now], map_persona_lease_row)?
                    .collect::<rusqlite::Result<Vec<_>>>()?)
            }
        }
    }

    /// Delta query for cluster sync. Returns every lease row (including
    /// tombstones) whose `updated_at > since`. Wire transport is not yet live;
    /// `sync_actor` reads this today so the count shows up in broadcast logs,
    /// and late-loser resolution is ready to engage when transport lands.
    #[allow(dead_code)] // wired when broadcast transport ships
    pub fn get_persona_wake_lease_deltas_since(
        &self,
        since: &str,
    ) -> Result<Vec<crate::sync::PersonaWakeLeaseDelta>> {
        let mut stmt = self.conn.prepare(
            "SELECT persona_id, signal_id, acquired_by_host, acquired_at, \
                    heartbeat_at, expires_at, deleted_at, updated_at \
             FROM persona_wake_leases \
             WHERE updated_at > ?1 \
             ORDER BY updated_at ASC",
        )?;
        let deltas = stmt
            .query_map(rusqlite::params![since], |row| {
                Ok(crate::sync::PersonaWakeLeaseDelta {
                    persona_id: row.get(0)?,
                    signal_id: row.get(1)?,
                    acquired_by_host: row.get(2)?,
                    acquired_at: row.get(3)?,
                    heartbeat_at: row.get(4)?,
                    expires_at: row.get(5)?,
                    deleted_at: row.get(6)?,
                    updated_at: row.get(7)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(deltas)
    }

    /// Apply an incoming lease delta from a peer. Resolution rules:
    ///
    /// - Tombstone (`deleted_at` set): LWW on `updated_at`. Newer wins.
    /// - Live lease vs. live lease for the same (persona, signal):
    ///   earlier `acquired_at` wins. The late-loser releases its local lease
    ///   so the spawned child is the only handler.
    /// - Live lease vs. no local row: insert the peer's lease as-is.
    ///
    /// Returns `Some(released)` with the locally-held `acquired_by_host` when
    /// this node was the late loser and its lease was downgraded to a
    /// tombstone. Callers can use this to stop the losing spawn.
    #[allow(dead_code)] // wired when broadcast transport ships
    pub fn apply_persona_wake_lease_delta(
        &self,
        delta: &crate::sync::PersonaWakeLeaseDelta,
    ) -> Result<Option<String>> {
        let tx = self.conn.unchecked_transaction()?;

        let local: Option<(String, String, Option<String>, String)> = tx
            .query_row(
                "SELECT acquired_by_host, acquired_at, deleted_at, updated_at \
                 FROM persona_wake_leases \
                 WHERE persona_id = ?1 AND signal_id = ?2",
                rusqlite::params![&delta.persona_id, &delta.signal_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .optional()?;

        let mut late_loser: Option<String> = None;

        match local {
            None => {
                tx.execute(
                    "INSERT INTO persona_wake_leases \
                     (persona_id, signal_id, acquired_by_host, acquired_at, heartbeat_at, \
                      expires_at, updated_at, deleted_at) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                    rusqlite::params![
                        &delta.persona_id,
                        &delta.signal_id,
                        &delta.acquired_by_host,
                        &delta.acquired_at,
                        &delta.heartbeat_at,
                        &delta.expires_at,
                        &delta.updated_at,
                        &delta.deleted_at,
                    ],
                )?;
            }
            Some((local_host, local_acquired, local_deleted, local_updated)) => {
                let delta_deleted = delta.deleted_at.is_some();
                let local_is_deleted = local_deleted.is_some();

                if delta_deleted || local_is_deleted {
                    // Tombstone involved: plain LWW on updated_at.
                    if delta.updated_at > local_updated {
                        tx.execute(
                            "UPDATE persona_wake_leases \
                             SET acquired_by_host = ?1, acquired_at = ?2, heartbeat_at = ?3, \
                                 expires_at = ?4, updated_at = ?5, deleted_at = ?6 \
                             WHERE persona_id = ?7 AND signal_id = ?8",
                            rusqlite::params![
                                &delta.acquired_by_host,
                                &delta.acquired_at,
                                &delta.heartbeat_at,
                                &delta.expires_at,
                                &delta.updated_at,
                                &delta.deleted_at,
                                &delta.persona_id,
                                &delta.signal_id,
                            ],
                        )?;
                    }
                } else if delta.acquired_at < local_acquired {
                    // Two live leases -- earlier acquired_at wins, regardless
                    // of updated_at ordering. Local is the late loser.
                    let now = Utc::now().to_rfc3339();
                    tx.execute(
                        "UPDATE persona_wake_leases \
                         SET acquired_by_host = ?1, acquired_at = ?2, heartbeat_at = ?3, \
                             expires_at = ?4, updated_at = ?5, deleted_at = NULL \
                         WHERE persona_id = ?6 AND signal_id = ?7",
                        rusqlite::params![
                            &delta.acquired_by_host,
                            &delta.acquired_at,
                            &delta.heartbeat_at,
                            &delta.expires_at,
                            &now,
                            &delta.persona_id,
                            &delta.signal_id,
                        ],
                    )?;
                    late_loser = Some(local_host);
                }
            }
        }

        tx.commit()?;
        Ok(late_loser)
    }

    /// Apply a peer's reflection delta with last-write-wins merge (#536).
    ///
    /// No local row: INSERT (embedding stays NULL -- each node computes its
    /// own embeddings, see the ReflectionDelta doc). Local row exists: the
    /// delta wins only when its effective timestamp (max of updated_at /
    /// deleted_at, falling back to created_at) is strictly newer than the
    /// local row's. Tombstones ride the same comparison.
    pub fn apply_reflection_delta(&self, delta: &crate::sync::ReflectionDelta) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;

        let local: Option<(String, Option<String>, Option<String>)> = tx
            .query_row(
                "SELECT created_at, updated_at, deleted_at FROM reflections WHERE id = ?1",
                rusqlite::params![&delta.id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?;

        let delta_ts = effective_sync_ts(&delta.created_at, &delta.updated_at, &delta.deleted_at);
        match local {
            None => {
                tx.execute(
                    "INSERT INTO reflections \
                     (id, repo, text, created_at, updated_at, deleted_at, audience, \
                      domain, tags, recall_count, last_recalled_at, parent_id) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
                    rusqlite::params![
                        &delta.id,
                        &delta.repo,
                        &delta.text,
                        &delta.created_at,
                        &delta.updated_at,
                        &delta.deleted_at,
                        &delta.audience,
                        &delta.domain,
                        &delta.tags,
                        &delta.recall_count,
                        &delta.last_recalled_at,
                        &delta.parent_id,
                    ],
                )?;
            }
            Some((local_created, local_updated, local_deleted)) => {
                let local_ts = effective_sync_ts(&local_created, &local_updated, &local_deleted);
                if delta_ts > local_ts {
                    tx.execute(
                        "UPDATE reflections SET repo = ?2, text = ?3, created_at = ?4, \
                         updated_at = ?5, deleted_at = ?6, audience = ?7, domain = ?8, \
                         tags = ?9, recall_count = ?10, last_recalled_at = ?11, parent_id = ?12 \
                         WHERE id = ?1",
                        rusqlite::params![
                            &delta.id,
                            &delta.repo,
                            &delta.text,
                            &delta.created_at,
                            &delta.updated_at,
                            &delta.deleted_at,
                            &delta.audience,
                            &delta.domain,
                            &delta.tags,
                            &delta.recall_count,
                            &delta.last_recalled_at,
                            &delta.parent_id,
                        ],
                    )?;
                }
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Apply a peer's card delta with last-write-wins merge (#536). Same
    /// LWW rule as [`Self::apply_reflection_delta`].
    pub fn apply_card_delta(&self, delta: &crate::sync::CardDelta) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;

        let local: Option<(String, Option<String>, Option<String>)> = tx
            .query_row(
                "SELECT created_at, updated_at, deleted_at FROM tasks WHERE id = ?1",
                rusqlite::params![&delta.id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?;

        let updated = Some(delta.updated_at.clone());
        let delta_ts = effective_sync_ts(&delta.created_at, &updated, &delta.deleted_at);
        match local {
            None => {
                tx.execute(
                    "INSERT INTO tasks \
                     (id, from_repo, to_repo, text, context, priority, status, note, labels, \
                      parent_card_id, source_url, source_type, sort_order, created_at, \
                      updated_at, deleted_at, assigned_at, started_at, completed_at, \
                      problem, solution, acceptance) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, \
                             ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22)",
                    rusqlite::params![
                        &delta.id,
                        &delta.from_repo,
                        &delta.to_repo,
                        &delta.text,
                        &delta.context,
                        &delta.priority,
                        &delta.status,
                        &delta.note,
                        &delta.labels,
                        &delta.parent_card_id,
                        &delta.source_url,
                        &delta.source_type,
                        &delta.sort_order,
                        &delta.created_at,
                        &delta.updated_at,
                        &delta.deleted_at,
                        &delta.assigned_at,
                        &delta.started_at,
                        &delta.completed_at,
                        &delta.problem,
                        &delta.solution,
                        &delta.acceptance,
                    ],
                )?;
            }
            Some((local_created, local_updated, local_deleted)) => {
                let local_ts = effective_sync_ts(&local_created, &local_updated, &local_deleted);
                if delta_ts > local_ts {
                    tx.execute(
                        "UPDATE tasks SET from_repo = ?2, to_repo = ?3, text = ?4, context = ?5, \
                         priority = ?6, status = ?7, note = ?8, labels = ?9, parent_card_id = ?10, \
                         source_url = ?11, source_type = ?12, sort_order = ?13, created_at = ?14, \
                         updated_at = ?15, deleted_at = ?16, assigned_at = ?17, started_at = ?18, \
                         completed_at = ?19, problem = ?20, solution = ?21, acceptance = ?22 \
                         WHERE id = ?1",
                        rusqlite::params![
                            &delta.id,
                            &delta.from_repo,
                            &delta.to_repo,
                            &delta.text,
                            &delta.context,
                            &delta.priority,
                            &delta.status,
                            &delta.note,
                            &delta.labels,
                            &delta.parent_card_id,
                            &delta.source_url,
                            &delta.source_type,
                            &delta.sort_order,
                            &delta.created_at,
                            &delta.updated_at,
                            &delta.deleted_at,
                            &delta.assigned_at,
                            &delta.started_at,
                            &delta.completed_at,
                            &delta.problem,
                            &delta.solution,
                            &delta.acceptance,
                        ],
                    )?;
                }
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Apply a peer's schedule delta with last-write-wins merge (#536). Same
    /// LWW rule as [`Self::apply_reflection_delta`].
    pub fn apply_schedule_delta(&self, delta: &crate::sync::ScheduleDelta) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;

        let local: Option<(String, Option<String>, Option<String>)> = tx
            .query_row(
                "SELECT created_at, updated_at, deleted_at FROM schedules WHERE id = ?1",
                rusqlite::params![&delta.id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?;

        let delta_ts = effective_sync_ts(&delta.created_at, &delta.updated_at, &delta.deleted_at);
        match local {
            None => {
                tx.execute(
                    "INSERT INTO schedules \
                     (id, name, cron, command, repo, enabled, last_run, next_run, created_at, \
                      updated_at, deleted_at, active_start, active_end) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
                    rusqlite::params![
                        &delta.id,
                        &delta.name,
                        &delta.cron,
                        &delta.command,
                        &delta.repo,
                        &delta.enabled,
                        &delta.last_run,
                        &delta.next_run,
                        &delta.created_at,
                        &delta.updated_at,
                        &delta.deleted_at,
                        &delta.active_start,
                        &delta.active_end,
                    ],
                )?;
            }
            Some((local_created, local_updated, local_deleted)) => {
                let local_ts = effective_sync_ts(&local_created, &local_updated, &local_deleted);
                if delta_ts > local_ts {
                    tx.execute(
                        "UPDATE schedules SET name = ?2, cron = ?3, command = ?4, repo = ?5, \
                         enabled = ?6, last_run = ?7, next_run = ?8, created_at = ?9, \
                         updated_at = ?10, deleted_at = ?11, active_start = ?12, active_end = ?13 \
                         WHERE id = ?1",
                        rusqlite::params![
                            &delta.id,
                            &delta.name,
                            &delta.cron,
                            &delta.command,
                            &delta.repo,
                            &delta.enabled,
                            &delta.last_run,
                            &delta.next_run,
                            &delta.created_at,
                            &delta.updated_at,
                            &delta.deleted_at,
                            &delta.active_start,
                            &delta.active_end,
                        ],
                    )?;
                }
            }
        }
        tx.commit()?;
        Ok(())
    }
}

/// Effective timestamp for sync LWW comparisons (#536): the latest of
/// updated_at / deleted_at, falling back to created_at when neither is set.
/// RFC3339 strings compare lexicographically in time order.
fn effective_sync_ts<'a>(
    created_at: &'a str,
    updated_at: &'a Option<String>,
    deleted_at: &'a Option<String>,
) -> &'a str {
    let mut best = created_at;
    if let Some(u) = updated_at.as_deref()
        && u > best
    {
        best = u;
    }
    if let Some(d) = deleted_at.as_deref()
        && d > best
    {
        best = d;
    }
    best
}

// -- wake_attempts (#487, part of #495) --------------------------------------
//
// Until #488 (sync delta) and #489/#490 (consumer wiring) land, these
// items are reachable only from tests. The wake_attempts module carries
// its own `#![allow(dead_code)]`; here we only need the allow on
// `map_wake_attempt_row` and the new `impl Database` block since the
// surrounding db.rs is mostly live.

#[allow(dead_code)]
fn map_wake_attempt_row(
    row: &rusqlite::Row<'_>,
) -> rusqlite::Result<crate::wake_attempts::WakeAttempt> {
    let state_str: String = row.get(4)?;
    let state = crate::wake_attempts::WakeAttemptState::parse_state(&state_str).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(
            4,
            rusqlite::types::Type::Text,
            Box::new(std::io::Error::other(format!("state decode: {}", e))),
        )
    })?;
    let signal_ids_json: String = row.get(3)?;
    let signal_ids: Vec<String> = serde_json::from_str(&signal_ids_json).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(3, rusqlite::types::Type::Text, Box::new(e))
    })?;
    let spawned_pid_i64: Option<i64> = row.get(7)?;
    Ok(crate::wake_attempts::WakeAttempt {
        attempt_id: row.get(0)?,
        persona_id: row.get(1)?,
        repo_name: row.get(2)?,
        signal_ids,
        state,
        acquired_by_host: row.get(5)?,
        acquired_at: row.get(6)?,
        spawned_pid: spawned_pid_i64.map(|v| v as u32),
        spawned_at: row.get(8)?,
        exit_observed_at: row.get(9)?,
        exited_at: row.get(10)?,
        exit_status: row.get(11)?,
        outcome: row.get(12)?,
        deleted_at: row.get(13)?,
        updated_at: row.get(14)?,
    })
}

#[allow(dead_code)] // wired by #488 / #489 / #490
impl Database {
    /// Insert a new wake_attempts row in the `queued` state. The caller
    /// is expected to mint a fresh UUIDv7 for `attempt_id`; reusing an
    /// existing id is a programming error and the PK constraint will
    /// surface as `LegionError::Database`.
    pub fn enqueue_wake_attempt(
        &self,
        attempt_id: &str,
        persona_id: &str,
        repo_name: &str,
        signal_ids: &[String],
    ) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        let signal_ids_json = serde_json::to_string(signal_ids)?;
        self.conn.execute(
            "INSERT INTO wake_attempts \
             (attempt_id, persona_id, repo_name, signal_ids, state, updated_at) \
             VALUES (?1, ?2, ?3, ?4, 'queued', ?5)",
            rusqlite::params![attempt_id, persona_id, repo_name, &signal_ids_json, &now],
        )?;
        Ok(())
    }

    /// Atomic claim. Returns `Ok(true)` when this host won the claim,
    /// `Ok(false)` when the row is either gone, already claimed by
    /// another host, or in a non-`queued` state. Mirrors
    /// `try_acquire_persona_lease`'s atomic UPDATE...WHERE pattern.
    pub fn try_claim_wake_attempt(&self, attempt_id: &str, host: &str) -> Result<bool> {
        let now = Utc::now().to_rfc3339();
        let updated = self.conn.execute(
            "UPDATE wake_attempts \
             SET state = 'claimed', acquired_by_host = ?1, acquired_at = ?2, updated_at = ?2 \
             WHERE attempt_id = ?3 \
               AND state = 'queued' \
               AND acquired_by_host IS NULL \
               AND deleted_at IS NULL",
            rusqlite::params![host, &now, attempt_id],
        )?;
        Ok(updated == 1)
    }

    /// FSM-enforced state transition. Two-layer safety: the Rust-side
    /// `can_transition_to` rejects illegal pairs before SQL, and the
    /// `state = from` predicate in the UPDATE rejects races where the
    /// row has already moved.
    ///
    /// Returns `Ok(())` on a successful transition;
    /// `IllegalWakeAttemptTransition` when the row's current state does
    /// not match `from` or the table forbids `from -> to`;
    /// `WakeAttemptNotFound` when the row is absent or soft-deleted.
    pub fn transition_wake_attempt(
        &self,
        attempt_id: &str,
        from: crate::wake_attempts::WakeAttemptState,
        to: crate::wake_attempts::WakeAttemptState,
    ) -> Result<()> {
        if !from.can_transition_to(to) {
            return self.illegal_transition(attempt_id, from, to);
        }

        let now = Utc::now().to_rfc3339();
        let updated = self.conn.execute(
            "UPDATE wake_attempts \
             SET state = ?1, updated_at = ?2 \
             WHERE attempt_id = ?3 AND state = ?4 AND deleted_at IS NULL",
            rusqlite::params![to.as_str(), &now, attempt_id, from.as_str()],
        )?;
        if updated == 1 {
            return Ok(());
        }
        // Distinguish "no such row" from "row exists in a different
        // state". One get_wake_attempt lookup either way -- the FSM
        // pre-check above already filtered out callable-but-illegal pairs.
        match self.get_wake_attempt(attempt_id)? {
            None => Err(LegionError::WakeAttemptNotFound(attempt_id.to_string())),
            Some(row) => Err(LegionError::IllegalWakeAttemptTransition {
                attempt_id: attempt_id.to_string(),
                from: from.as_str().to_string(),
                to: to.as_str().to_string(),
                current: row.state.as_str().to_string(),
            }),
        }
    }

    fn illegal_transition(
        &self,
        attempt_id: &str,
        from: crate::wake_attempts::WakeAttemptState,
        to: crate::wake_attempts::WakeAttemptState,
    ) -> Result<()> {
        let current = self
            .get_wake_attempt(attempt_id)?
            .map(|r| r.state.as_str().to_string())
            .unwrap_or_else(|| "<missing>".to_string());
        Err(LegionError::IllegalWakeAttemptTransition {
            attempt_id: attempt_id.to_string(),
            from: from.as_str().to_string(),
            to: to.as_str().to_string(),
            current,
        })
    }

    /// Record the PID of a spawned PTY child. Called after the child
    /// reaches `Spawning`; the value is a hint for `kill -0` style
    /// liveness probes, not the authority on completion (the PTY fd is).
    pub fn set_wake_attempt_pid(&self, attempt_id: &str, pid: u32) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        let updated = self.conn.execute(
            "UPDATE wake_attempts \
             SET spawned_pid = ?1, spawned_at = ?2, updated_at = ?2 \
             WHERE attempt_id = ?3 AND deleted_at IS NULL",
            rusqlite::params![pid as i64, &now, attempt_id],
        )?;
        if updated == 1 {
            Ok(())
        } else {
            Err(LegionError::WakeAttemptNotFound(attempt_id.to_string()))
        }
    }

    /// Stop-hook expediter: mark `exit_observed_at` so the reaper can
    /// short-circuit its next poll cycle. NOT authoritative -- the
    /// reaper still confirms via PTY EOF + PID-poll before writing a
    /// terminal state. The hook may legitimately never fire (8-block
    /// stop-hook cap in Claude Code 2.1.143).
    pub fn mark_wake_attempt_exit_observed(&self, attempt_id: &str) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        let updated = self.conn.execute(
            "UPDATE wake_attempts \
             SET exit_observed_at = ?1, updated_at = ?1 \
             WHERE attempt_id = ?2 AND deleted_at IS NULL",
            rusqlite::params![&now, attempt_id],
        )?;
        if updated == 1 {
            Ok(())
        } else {
            Err(LegionError::WakeAttemptNotFound(attempt_id.to_string()))
        }
    }

    /// Terminal classification + outcome stamp. Sets `exited_at`,
    /// `exit_status`, `outcome`, and transitions the row to a terminal
    /// FSM state based on `exit_status` (ok -> Done, error -> Failed,
    /// killed -> Failed). The reaper may short-circuit from any
    /// in-flight state when a stop hook + PID-dead race collapses the
    /// lifecycle, so we accept any of claimed/spawning/running/exiting
    /// as the source. Terminal-is-sticky stays load-bearing: the
    /// WHERE clause excludes already-terminal rows so a late hook
    /// cannot rewrite a settled outcome (the call surfaces as
    /// `WakeAttemptNotFound` so the caller notices).
    pub fn record_wake_attempt_outcome(
        &self,
        attempt_id: &str,
        exit_status: &str,
        outcome: &str,
    ) -> Result<()> {
        let terminal = match exit_status {
            "ok" => "done",
            "error" | "killed" => "failed",
            other => {
                return Err(LegionError::IllegalWakeAttemptTransition {
                    attempt_id: attempt_id.to_string(),
                    from: "<reaper>".to_string(),
                    to: other.to_string(),
                    current: "<unknown exit_status>".to_string(),
                });
            }
        };
        let now = Utc::now().to_rfc3339();
        let updated = self.conn.execute(
            "UPDATE wake_attempts \
             SET state = ?1, exited_at = ?2, exit_status = ?3, outcome = ?4, updated_at = ?2 \
             WHERE attempt_id = ?5 \
               AND state IN ('claimed', 'spawning', 'running', 'exiting') \
               AND deleted_at IS NULL",
            rusqlite::params![terminal, &now, exit_status, outcome, attempt_id],
        )?;
        if updated == 1 {
            return Ok(());
        }
        // Distinguish "no such row" from "row exists but is already
        // terminal (or queued)". Same diagnostic shape as
        // transition_wake_attempt -- callers can branch on the variant
        // without retrying a corruption case as if it were absence.
        match self.get_wake_attempt(attempt_id)? {
            None => Err(LegionError::WakeAttemptNotFound(attempt_id.to_string())),
            Some(row) => Err(LegionError::IllegalWakeAttemptTransition {
                attempt_id: attempt_id.to_string(),
                from: "<reaper>".to_string(),
                to: terminal.to_string(),
                current: row.state.as_str().to_string(),
            }),
        }
    }

    /// Strictly host-scoped orphan scan: rows owned by `self_host` that
    /// are in any pre-terminal in-flight state. The host filter is
    /// load-bearing -- a two-node sweep race could reap a peer's still-
    /// running attempt and stick its persona lease until TTL.
    pub fn list_local_orphans(
        &self,
        self_host: &str,
    ) -> Result<Vec<crate::wake_attempts::WakeAttempt>> {
        let mut stmt = self.conn.prepare(
            "SELECT attempt_id, persona_id, repo_name, signal_ids, state, \
                    acquired_by_host, acquired_at, spawned_pid, spawned_at, \
                    exit_observed_at, exited_at, exit_status, outcome, \
                    deleted_at, updated_at \
             FROM wake_attempts \
             WHERE acquired_by_host = ?1 \
               AND state IN ('claimed', 'spawning', 'running', 'exiting') \
               AND deleted_at IS NULL \
             ORDER BY acquired_at ASC",
        )?;
        Ok(stmt
            .query_map(rusqlite::params![self_host], map_wake_attempt_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Fetch a single row by id (including soft-deleted ones, for sync
    /// reconciliation). Returns `Ok(None)` when no row exists.
    pub fn get_wake_attempt(
        &self,
        attempt_id: &str,
    ) -> Result<Option<crate::wake_attempts::WakeAttempt>> {
        let mut stmt = self.conn.prepare(
            "SELECT attempt_id, persona_id, repo_name, signal_ids, state, \
                    acquired_by_host, acquired_at, spawned_pid, spawned_at, \
                    exit_observed_at, exited_at, exit_status, outcome, \
                    deleted_at, updated_at \
             FROM wake_attempts \
             WHERE attempt_id = ?1",
        )?;
        let row = stmt
            .query_row(rusqlite::params![attempt_id], map_wake_attempt_row)
            .optional()?;
        Ok(row)
    }

    /// Apply an incoming sync delta with state-aware conflict resolution
    /// (#488). Returns `Ok(true)` when the delta was applied or inserted,
    /// `Ok(false)` when rejected (no-op / regression / forward-incompat).
    ///
    /// Conflict rule (in priority order):
    ///
    /// 1. Unknown state literal -> log + reject (forward-incompat from a
    ///    newer peer; do not panic).
    /// 2. No local row -> insert.
    /// 3. Tombstone involved (either side has `deleted_at`) -> plain LWW
    ///    on `updated_at`.
    /// 4. Local terminal + incoming non-terminal -> REJECT. Terminal is
    ///    sticky; a peer observing an earlier state must not regress us.
    /// 5. Both terminal but disagree on state -> keep the row with the
    ///    later `exited_at`; on tie, deterministic tiebreak on
    ///    `acquired_by_host` (lexicographic, lower wins).
    /// 6. Otherwise -> LWW on `updated_at`.
    pub fn apply_wake_attempt_delta(&self, delta: &crate::sync::WakeAttemptDelta) -> Result<bool> {
        // Forward-incompat guard: unknown state literal does not panic.
        let delta_state = match crate::wake_attempts::WakeAttemptState::parse_state(&delta.state) {
            Ok(s) => s,
            Err(e) => {
                eprintln!(
                    "[legion sync] wake_attempt_delta rejected: unknown state {:?} \
                         (attempt_id={}, err={})",
                    delta.state, delta.attempt_id, e
                );
                return Ok(false);
            }
        };

        let tx = self.conn.unchecked_transaction()?;
        let local = tx
            .query_row(
                "SELECT state, exited_at, updated_at, deleted_at, acquired_by_host \
                 FROM wake_attempts WHERE attempt_id = ?1",
                rusqlite::params![&delta.attempt_id],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, Option<String>>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, Option<String>>(3)?,
                        r.get::<_, Option<String>>(4)?,
                    ))
                },
            )
            .optional()?;

        let signal_ids_json = serde_json::to_string(&delta.signal_ids)?;

        let applied = match local {
            None => {
                tx.execute(
                    "INSERT INTO wake_attempts \
                     (attempt_id, persona_id, repo_name, signal_ids, state, \
                      acquired_by_host, acquired_at, spawned_pid, spawned_at, \
                      exit_observed_at, exited_at, exit_status, outcome, \
                      deleted_at, updated_at) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
                    rusqlite::params![
                        &delta.attempt_id,
                        &delta.persona_id,
                        &delta.repo_name,
                        &signal_ids_json,
                        &delta.state,
                        &delta.acquired_by_host,
                        &delta.acquired_at,
                        delta.spawned_pid.map(|v| v as i64),
                        &delta.spawned_at,
                        &delta.exit_observed_at,
                        &delta.exited_at,
                        &delta.exit_status,
                        &delta.outcome,
                        &delta.deleted_at,
                        &delta.updated_at,
                    ],
                )?;
                true
            }
            Some((local_state_str, local_exited, local_updated, local_deleted, local_host)) => {
                let local_state =
                    match crate::wake_attempts::WakeAttemptState::parse_state(&local_state_str) {
                        Ok(s) => s,
                        Err(_) => {
                            // Local row is corrupt -- treat any incoming
                            // well-formed delta as the truth (forward-only
                            // schema recovery). LWW still applies.
                            if delta.updated_at <= local_updated {
                                tx.commit()?;
                                return Ok(false);
                            }
                            self.upsert_wake_attempt_overwrite(&tx, delta, &signal_ids_json)?;
                            tx.commit()?;
                            return Ok(true);
                        }
                    };

                let tombstone_involved = local_deleted.is_some() || delta.deleted_at.is_some();
                let local_terminal = local_state.is_terminal();
                let delta_terminal = delta_state.is_terminal();

                if tombstone_involved {
                    // Plain LWW on updated_at for tombstones.
                    if delta.updated_at > local_updated {
                        self.upsert_wake_attempt_overwrite(&tx, delta, &signal_ids_json)?;
                        true
                    } else {
                        false
                    }
                } else if local_terminal && !delta_terminal {
                    // Terminal-is-sticky: reject the regression. The
                    // local row keeps its terminal state regardless of
                    // updated_at -- a newer non-terminal write is a
                    // happens-before violation by definition.
                    false
                } else if local_terminal && delta_terminal {
                    if local_state == delta_state {
                        // Both same terminal -- LWW on updated_at to
                        // accept fresher metadata (outcome label, etc).
                        if delta.updated_at > local_updated {
                            self.upsert_wake_attempt_overwrite(&tx, delta, &signal_ids_json)?;
                            true
                        } else {
                            false
                        }
                    } else {
                        // Both terminal but disagree -- keep the later
                        // exited_at; on tie, deterministic tiebreak on
                        // acquired_by_host (lower lexicographic wins).
                        let delta_wins = match (&delta.exited_at, &local_exited) {
                            (Some(d), Some(l)) if d != l => d > l,
                            (Some(_), None) => true,
                            (None, Some(_)) => false,
                            _ => {
                                // exited_at tie -- break on host id.
                                match (&delta.acquired_by_host, &local_host) {
                                    (Some(d), Some(l)) => d < l,
                                    (Some(_), None) => true,
                                    _ => false,
                                }
                            }
                        };
                        if delta_wins {
                            self.upsert_wake_attempt_overwrite(&tx, delta, &signal_ids_json)?;
                            true
                        } else {
                            false
                        }
                    }
                } else if delta.updated_at > local_updated {
                    // Live row, neither terminal -- plain LWW.
                    self.upsert_wake_attempt_overwrite(&tx, delta, &signal_ids_json)?;
                    true
                } else {
                    false
                }
            }
        };

        tx.commit()?;
        Ok(applied)
    }

    fn upsert_wake_attempt_overwrite(
        &self,
        tx: &rusqlite::Transaction<'_>,
        delta: &crate::sync::WakeAttemptDelta,
        signal_ids_json: &str,
    ) -> Result<()> {
        tx.execute(
            "UPDATE wake_attempts \
             SET persona_id = ?1, repo_name = ?2, signal_ids = ?3, state = ?4, \
                 acquired_by_host = ?5, acquired_at = ?6, spawned_pid = ?7, \
                 spawned_at = ?8, exit_observed_at = ?9, exited_at = ?10, \
                 exit_status = ?11, outcome = ?12, deleted_at = ?13, updated_at = ?14 \
             WHERE attempt_id = ?15",
            rusqlite::params![
                &delta.persona_id,
                &delta.repo_name,
                signal_ids_json,
                &delta.state,
                &delta.acquired_by_host,
                &delta.acquired_at,
                delta.spawned_pid.map(|v| v as i64),
                &delta.spawned_at,
                &delta.exit_observed_at,
                &delta.exited_at,
                &delta.exit_status,
                &delta.outcome,
                &delta.deleted_at,
                &delta.updated_at,
                &delta.attempt_id,
            ],
        )?;
        Ok(())
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

    /// Return the most recent gate row for `skill` across all commits, if any.
    ///
    /// Unlike `get_quality_gate`, this ignores the commit hash. Used by the
    /// verify gate (#520): a verify verdict is keyed on the card
    /// (`legion-verify:<card_id>`) and must still satisfy the ->Done check even
    /// when `legion done` runs on a different commit than verify did (e.g.
    /// after the branch merged). Staleness is the caller's concern -- re-verify
    /// after material changes.
    pub fn get_latest_quality_gate_by_skill(&self, skill: &str) -> Result<Option<QualityGateRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, branch, commit_hash, skill, result, findings_count, details, created_at \
             FROM quality_gates \
             WHERE skill = ?1 \
             ORDER BY created_at DESC \
             LIMIT 1",
        )?;
        let mut rows = stmt.query_map(rusqlite::params![skill], |row| {
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

    /// Load the autonomy budget for `repo`, if one has been recorded.
    ///
    /// An unparseable stored `window_start` (should never happen -- we always
    /// write RFC3339) is treated as "no budget" so the caller starts a fresh
    /// window rather than erroring on its own corrupt row.
    pub fn get_autonomy_budget(
        &self,
        repo: &str,
    ) -> Result<Option<crate::autonomy::AutonomyBudget>> {
        let mut stmt = self
            .conn
            .prepare("SELECT window_start, spent, ceiling FROM autonomy_budget WHERE repo = ?1")?;
        let mut rows = stmt.query_map(rusqlite::params![repo], |row| {
            let window_start: String = row.get(0)?;
            let spent: i64 = row.get(1)?;
            let ceiling: i64 = row.get(2)?;
            Ok((window_start, spent, ceiling))
        })?;
        match rows.next() {
            Some(Ok((ws, spent, ceiling))) => match chrono::DateTime::parse_from_rfc3339(&ws) {
                Ok(dt) => Ok(Some(crate::autonomy::AutonomyBudget {
                    window_start: dt.with_timezone(&Utc),
                    spent: spent.unsigned_abs(),
                    ceiling: ceiling.unsigned_abs(),
                })),
                Err(_) => Ok(None),
            },
            Some(Err(e)) => Err(LegionError::Database(e)),
            None => Ok(None),
        }
    }

    /// Insert or update the autonomy budget for `repo`.
    pub fn upsert_autonomy_budget(
        &self,
        repo: &str,
        budget: &crate::autonomy::AutonomyBudget,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT INTO autonomy_budget (repo, window_start, spent, ceiling, updated_at) \
             VALUES (?1, ?2, ?3, ?4, ?5) \
             ON CONFLICT(repo) DO UPDATE SET \
                window_start = excluded.window_start, \
                spent = excluded.spent, \
                ceiling = excluded.ceiling, \
                updated_at = excluded.updated_at",
            rusqlite::params![
                repo,
                budget.window_start.to_rfc3339(),
                budget.spent as i64,
                budget.ceiling as i64,
                Utc::now().to_rfc3339(),
            ],
        )?;
        Ok(())
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

/// Result of tombstone cleanup operation.
#[derive(Debug, Default)]
pub struct TombstoneCleanupResult {
    pub reflections: u64,
    pub tasks: u64,
    pub schedules: u64,
    pub uncertainty_predictions: u64,
    pub uncertainty_calibration_snapshots: u64,
}

impl TombstoneCleanupResult {
    pub fn total(&self) -> u64 {
        self.reflections
            + self.tasks
            + self.schedules
            + self.uncertainty_predictions
            + self.uncertainty_calibration_snapshots
    }

    pub fn is_empty(&self) -> bool {
        self.total() == 0
    }
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

impl Database {
    /// Write or overwrite a SCIP index row identified by (repo, lang).
    /// Idempotent on `content_hash`: when the stored hash matches the
    /// incoming one, only `updated_at` is bumped. Otherwise the blob,
    /// hash, and timestamp are all rewritten.
    pub fn upsert_scip_index(&self, index: &crate::scip::ScipIndex) -> Result<()> {
        let existing: Option<(String, String)> = self
            .conn
            .query_row(
                "SELECT id, content_hash FROM scip_indexes
                 WHERE repo = ?1 AND lang = ?2 AND deleted_at IS NULL",
                rusqlite::params![&index.repo, &index.lang],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;

        match existing {
            Some((id, hash)) if hash == index.content_hash => {
                self.conn.execute(
                    "UPDATE scip_indexes SET updated_at = ?1 WHERE id = ?2",
                    rusqlite::params![&index.updated_at, &id],
                )?;
            }
            Some((id, _)) => {
                self.conn.execute(
                    "UPDATE scip_indexes
                     SET content_hash = ?1, blob = ?2, updated_at = ?3
                     WHERE id = ?4",
                    rusqlite::params![&index.content_hash, &index.blob, &index.updated_at, &id],
                )?;
            }
            None => {
                self.conn.execute(
                    "INSERT INTO scip_indexes
                     (id, repo, lang, content_hash, blob, updated_at, deleted_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, NULL)",
                    rusqlite::params![
                        &index.id,
                        &index.repo,
                        &index.lang,
                        &index.content_hash,
                        &index.blob,
                        &index.updated_at,
                    ],
                )?;
            }
        }
        Ok(())
    }

    /// Retrieve the active stored index for (repo, lang).
    /// Read path consumed by the SCIP query CLI in #282.
    #[allow(dead_code)]
    pub fn get_scip_index(&self, repo: &str, lang: &str) -> Result<Option<crate::scip::ScipIndex>> {
        let row = self
            .conn
            .query_row(
                "SELECT id, repo, lang, content_hash, blob, updated_at, deleted_at
                 FROM scip_indexes
                 WHERE repo = ?1 AND lang = ?2 AND deleted_at IS NULL",
                rusqlite::params![repo, lang],
                |row| {
                    Ok(crate::scip::ScipIndex {
                        id: row.get(0)?,
                        repo: row.get(1)?,
                        lang: row.get(2)?,
                        content_hash: row.get(3)?,
                        blob: row.get(4)?,
                        updated_at: row.get(5)?,
                        deleted_at: row.get(6)?,
                    })
                },
            )
            .optional()?;
        Ok(row)
    }

    /// List non-deleted SCIP indexes filtered by optional repo and language.
    /// `None` on either field disables that filter; `(None, None)` returns
    /// every live index. Used by `legion sym` query dispatch (#282) so a
    /// single call covers `--repo`/`--lang` scoping.
    pub fn list_scip_indexes_filtered(
        &self,
        repo: Option<&str>,
        lang: Option<&str>,
    ) -> Result<Vec<crate::scip::ScipIndex>> {
        let mut sql = String::from(
            "SELECT id, repo, lang, content_hash, blob, updated_at, deleted_at
             FROM scip_indexes
             WHERE deleted_at IS NULL",
        );
        let mut params: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
        if let Some(r) = repo {
            sql.push_str(" AND repo = ?");
            params.push(Box::new(r.to_string()));
        }
        if let Some(l) = lang {
            sql.push_str(" AND lang = ?");
            params.push(Box::new(l.to_string()));
        }
        sql.push_str(" ORDER BY repo, lang");

        let mut stmt = self.conn.prepare(&sql)?;
        let param_refs: Vec<&dyn rusqlite::ToSql> = params.iter().map(|p| p.as_ref()).collect();
        let rows = stmt
            .query_map(rusqlite::params_from_iter(param_refs), |row| {
                Ok(crate::scip::ScipIndex {
                    id: row.get(0)?,
                    repo: row.get(1)?,
                    lang: row.get(2)?,
                    content_hash: row.get(3)?,
                    blob: row.get(4)?,
                    updated_at: row.get(5)?,
                    deleted_at: row.get(6)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// List all non-deleted SCIP indexes for a repo.
    /// Read path consumed by `legion consult --symbol` in #285.
    #[allow(dead_code)]
    pub fn list_scip_indexes(&self, repo: &str) -> Result<Vec<crate::scip::ScipIndex>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, repo, lang, content_hash, blob, updated_at, deleted_at
             FROM scip_indexes
             WHERE repo = ?1 AND deleted_at IS NULL
             ORDER BY lang",
        )?;
        let rows = stmt
            .query_map(rusqlite::params![repo], |row| {
                Ok(crate::scip::ScipIndex {
                    id: row.get(0)?,
                    repo: row.get(1)?,
                    lang: row.get(2)?,
                    content_hash: row.get(3)?,
                    blob: row.get(4)?,
                    updated_at: row.get(5)?,
                    deleted_at: row.get(6)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }
}

// -- watch heartbeat (#581) --------------------------------------------------

/// A persisted liveness record written by the watch daemon on each health tick.
///
/// One row per host, keyed by hostname. Reading this row tells an operator
/// whether the daemon is alive (beat is recent), stale (beat is old), or
/// absent (no row). The `version` field is the binary's CARGO_PKG_VERSION at
/// the time the row was written, which may differ from a fresh `legion --version`
/// invocation when the binary was replaced but the daemon process was not restarted.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct WatchHeartbeat {
    /// Hostname that wrote this row (primary key).
    pub host: String,
    /// PID of the daemon process that wrote this row.
    pub pid: u32,
    /// `CARGO_PKG_VERSION` of the binary the daemon loaded at startup.
    pub version: String,
    /// Number of repos in watch.toml at the time of the beat.
    pub repo_count: u32,
    /// Optional human-readable summary of the most recent poll cycle's spawns.
    pub last_spawn_summary: Option<String>,
    /// ISO-8601 timestamp of the most recent upsert.
    pub updated_at: String,
}

impl Database {
    /// Upsert the heartbeat row for this host.
    ///
    /// Called by the daemon on every health_interval tick. The ON CONFLICT
    /// clause updates every column so stale values never accumulate.
    pub fn upsert_watch_heartbeat(
        &self,
        host: &str,
        pid: u32,
        version: &str,
        repo_count: u32,
        last_spawn_summary: Option<&str>,
    ) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        self.conn.execute(
            "INSERT INTO watch_heartbeat \
             (host, pid, version, repo_count, last_spawn_summary, updated_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6) \
             ON CONFLICT(host) DO UPDATE SET \
                 pid = excluded.pid, \
                 version = excluded.version, \
                 repo_count = excluded.repo_count, \
                 last_spawn_summary = excluded.last_spawn_summary, \
                 updated_at = excluded.updated_at",
            rusqlite::params![
                host,
                pid as i64,
                version,
                repo_count as i64,
                last_spawn_summary,
                &now
            ],
        )?;
        Ok(())
    }

    /// Read the heartbeat row for a specific host, or any host when `host` is `None`.
    ///
    /// When `host` is `None`, returns the most recently updated row across all hosts.
    /// Returns `Ok(None)` when no heartbeat has ever been written.
    pub fn get_watch_heartbeat(&self, host: Option<&str>) -> Result<Option<WatchHeartbeat>> {
        let map_row = |row: &rusqlite::Row<'_>| -> rusqlite::Result<WatchHeartbeat> {
            let pid_i64: i64 = row.get(1)?;
            let repo_count_i64: i64 = row.get(3)?;
            Ok(WatchHeartbeat {
                host: row.get(0)?,
                pid: pid_i64 as u32,
                version: row.get(2)?,
                repo_count: repo_count_i64 as u32,
                last_spawn_summary: row.get(4)?,
                updated_at: row.get(5)?,
            })
        };
        match host {
            Some(h) => self
                .conn
                .query_row(
                    "SELECT host, pid, version, repo_count, last_spawn_summary, updated_at \
                     FROM watch_heartbeat WHERE host = ?1",
                    rusqlite::params![h],
                    map_row,
                )
                .optional()
                .map_err(LegionError::Database),
            None => self
                .conn
                .query_row(
                    "SELECT host, pid, version, repo_count, last_spawn_summary, updated_at \
                     FROM watch_heartbeat ORDER BY updated_at DESC LIMIT 1",
                    [],
                    map_row,
                )
                .optional()
                .map_err(LegionError::Database),
        }
    }

    /// Return the N most recent wake_attempts rows, ordered newest-first.
    ///
    /// Used by `legion watch status` to show a terse wake activity summary.
    /// All states are included (terminal and in-flight) so the operator sees
    /// the full recent history, not just live wakes.
    pub fn recent_wake_attempts(
        &self,
        limit: u32,
    ) -> Result<Vec<crate::wake_attempts::WakeAttempt>> {
        let mut stmt = self.conn.prepare(
            "SELECT attempt_id, persona_id, repo_name, signal_ids, state, \
                    acquired_by_host, acquired_at, spawned_pid, spawned_at, \
                    exit_observed_at, exited_at, exit_status, outcome, \
                    deleted_at, updated_at \
             FROM wake_attempts \
             WHERE deleted_at IS NULL \
             ORDER BY updated_at DESC \
             LIMIT ?1",
        )?;
        Ok(stmt
            .query_map(rusqlite::params![limit as i64], map_wake_attempt_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?)
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
    fn updated_at_set_on_insert() {
        let db = test_db();
        let r = db
            .insert_reflection("test", "test reflection", "self")
            .unwrap();
        // updated_at should be set to created_at on insert
        assert!(r.updated_at.is_some());
        assert_eq!(r.updated_at.as_ref().unwrap(), &r.created_at);
    }

    #[test]
    fn updated_at_refreshed_on_boost() {
        let db = test_db();
        let r = db
            .insert_reflection("test", "test reflection", "self")
            .unwrap();
        let original_updated_at = r.updated_at.clone();

        // Small delay to ensure timestamp differs
        std::thread::sleep(std::time::Duration::from_millis(10));

        db.boost_reflection(&r.id).unwrap();

        let boosted = db.get_reflection_by_id(&r.id).unwrap().unwrap();
        assert!(boosted.updated_at.is_some());
        // updated_at should be later than the original
        assert!(boosted.updated_at.unwrap() > original_updated_at.unwrap());
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

        // expires_at far in the future so the #376 decay filter does not
        // exclude these rows. The test predates the decay column.
        let far_future = "2099-01-01T00:00:00+00:00";
        db.conn
            .execute(
                "INSERT INTO reflections (id, repo, text, created_at, audience, expires_at) \
                 VALUES (?1, 'kelex', 'tied A', ?2, 'team', ?3)",
                rusqlite::params![id_a, shared_ts, far_future],
            )
            .unwrap();
        db.conn
            .execute(
                "INSERT INTO reflections (id, repo, text, created_at, audience, expires_at) \
                 VALUES (?1, 'kelex', 'tied B', ?2, 'team', ?3)",
                rusqlite::params![id_b, shared_ts, far_future],
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
    fn get_board_read_cursor_none_for_fresh_repo() {
        let db = test_db();
        assert!(db.get_board_read_cursor("kessel").unwrap().is_none());
    }

    #[test]
    fn advance_board_read_cursor_creates_then_moves_forward() {
        let db = test_db();
        db.advance_board_read_cursor("kessel", "2026-05-04T01:00:00+00:00", "id-a")
            .unwrap();
        assert_eq!(
            db.get_board_read_cursor("kessel").unwrap(),
            Some(("2026-05-04T01:00:00+00:00".to_string(), "id-a".to_string()))
        );

        // Forward move accepted.
        db.advance_board_read_cursor("kessel", "2026-05-04T02:00:00+00:00", "id-b")
            .unwrap();
        assert_eq!(
            db.get_board_read_cursor("kessel").unwrap(),
            Some(("2026-05-04T02:00:00+00:00".to_string(), "id-b".to_string()))
        );
    }

    #[test]
    fn advance_board_read_cursor_refuses_backward_move() {
        let db = test_db();
        db.advance_board_read_cursor("kessel", "2026-05-04T02:00:00+00:00", "id-b")
            .unwrap();
        // Older timestamp must not overwrite.
        db.advance_board_read_cursor("kessel", "2026-05-04T01:00:00+00:00", "id-a")
            .unwrap();
        assert_eq!(
            db.get_board_read_cursor("kessel").unwrap(),
            Some(("2026-05-04T02:00:00+00:00".to_string(), "id-b".to_string()))
        );
    }

    #[test]
    fn advance_board_read_cursor_breaks_tie_on_id() {
        let db = test_db();
        let ts = "2026-05-04T02:00:00+00:00";
        db.advance_board_read_cursor("kessel", ts, "id-a").unwrap();
        // Same timestamp, larger id -> accepted.
        db.advance_board_read_cursor("kessel", ts, "id-b").unwrap();
        assert_eq!(
            db.get_board_read_cursor("kessel").unwrap(),
            Some((ts.to_string(), "id-b".to_string()))
        );
        // Same timestamp, smaller id -> refused.
        db.advance_board_read_cursor("kessel", ts, "id-a").unwrap();
        assert_eq!(
            db.get_board_read_cursor("kessel").unwrap(),
            Some((ts.to_string(), "id-b".to_string()))
        );
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
    fn get_identity_roots_excludes_chain_children() {
        let db = test_db();
        let root = db
            .insert_reflection_with_meta(
                "legion",
                "root identity",
                "self",
                &ReflectionMeta {
                    domain: Some("identity".into()),
                    ..Default::default()
                },
            )
            .unwrap();
        let _child = db
            .insert_reflection_with_meta(
                "legion",
                "chain child identity",
                "self",
                &ReflectionMeta {
                    domain: Some("identity".into()),
                    parent_id: Some(root.id.clone()),
                    ..Default::default()
                },
            )
            .unwrap();
        let orphan = db
            .insert_reflection_with_meta(
                "legion",
                "orphan identity",
                "self",
                &ReflectionMeta {
                    domain: Some("identity".into()),
                    ..Default::default()
                },
            )
            .unwrap();

        let roots = db.get_identity_roots("legion", 10).unwrap();
        let ids: Vec<&str> = roots.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(roots.len(), 2);
        assert!(ids.contains(&root.id.as_str()));
        assert!(ids.contains(&orphan.id.as_str()));
    }

    #[test]
    fn get_identity_roots_filters_by_repo_and_domain() {
        let db = test_db();
        let _other_domain = db
            .insert_reflection_with_meta(
                "legion",
                "not identity",
                "self",
                &ReflectionMeta {
                    domain: Some("architecture".into()),
                    ..Default::default()
                },
            )
            .unwrap();
        let _other_repo = db
            .insert_reflection_with_meta(
                "kelex",
                "wrong repo",
                "self",
                &ReflectionMeta {
                    domain: Some("identity".into()),
                    ..Default::default()
                },
            )
            .unwrap();
        let target = db
            .insert_reflection_with_meta(
                "legion",
                "right one",
                "self",
                &ReflectionMeta {
                    domain: Some("identity".into()),
                    ..Default::default()
                },
            )
            .unwrap();

        let roots = db.get_identity_roots("legion", 10).unwrap();
        assert_eq!(roots.len(), 1);
        assert_eq!(roots[0].id, target.id);
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
            updated_at: None,
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
            updated_at: None,
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
            updated_at: None,
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

    #[test]
    fn latest_quality_gate_by_skill_ignores_commit() {
        // #520: the verify gate is card-keyed (legion-verify:<card>) and must
        // resolve regardless of the commit it was recorded on, since `legion
        // done` may run on a different commit than verify did.
        let db = test_db();
        let skill = "legion-verify:card-7";
        db.record_quality_gate("feat/x", "commit-old", skill, "issues", 1, None)
            .unwrap();
        db.record_quality_gate("main", "commit-new", skill, "clean", 0, None)
            .unwrap();

        let latest = db
            .get_latest_quality_gate_by_skill(skill)
            .unwrap()
            .expect("a verify gate should exist for the card");
        assert_eq!(
            latest.result, "clean",
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

    #[test]
    fn autonomy_budget_roundtrips_and_upserts_by_repo() {
        let db = test_db();
        assert!(db.get_autonomy_budget("kelex").unwrap().is_none());

        let budget = crate::autonomy::AutonomyBudget {
            window_start: chrono::DateTime::parse_from_rfc3339("2026-05-01T00:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
            spent: 3,
            ceiling: 15,
        };
        db.upsert_autonomy_budget("kelex", &budget).unwrap();
        assert_eq!(
            db.get_autonomy_budget("kelex").unwrap().expect("exists"),
            budget
        );

        // Upsert is keyed on repo: a second write updates in place.
        let updated = crate::autonomy::AutonomyBudget {
            spent: 9,
            ..budget.clone()
        };
        db.upsert_autonomy_budget("kelex", &updated).unwrap();
        let got = db.get_autonomy_budget("kelex").unwrap().unwrap();
        assert_eq!(got.spent, 9);
        assert_eq!(got.ceiling, 15);

        // Isolated by repo.
        assert!(db.get_autonomy_budget("smugglr").unwrap().is_none());
    }

    #[test]
    fn delete_reflection_removes_row_and_returns_deleted() {
        let db = test_db();

        // Insert a reflection via the real path so the schema columns
        // are all populated exactly as production would.
        let inserted = db
            .insert_reflection("shingle", "stale workaround doctrine", "self")
            .unwrap();

        // Confirm it is visible before the delete.
        assert!(db.get_reflection_by_id(&inserted.id).unwrap().is_some());

        // Delete, confirm the returned row matches what was stored.
        let deleted = db.delete_reflection(&inserted.id).expect("delete");
        assert_eq!(deleted.id, inserted.id);
        assert_eq!(deleted.repo, "shingle");
        assert_eq!(deleted.text, "stale workaround doctrine");

        // Gone from the table.
        assert!(db.get_reflection_by_id(&inserted.id).unwrap().is_none());

        // Second delete on the same id returns ReflectionNotFound,
        // not silent success.
        let result = db.delete_reflection(&inserted.id);
        assert!(
            matches!(result, Err(LegionError::ReflectionNotFound(_))),
            "expected ReflectionNotFound, got {:?}",
            result
        );
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
    fn soft_delete_reflection_hides_from_queries() {
        let db = test_db();

        // Insert a reflection.
        let r = db
            .insert_reflection("test-repo", "soft delete test reflection", "self")
            .unwrap();
        let id = r.id;
        assert!(db.get_reflection_by_id(&id).unwrap().is_some());

        // Soft delete it.
        let deleted = db.soft_delete_reflection(&id).unwrap();
        assert!(deleted, "soft_delete_reflection should return true");

        // The reflection should now be invisible to normal queries.
        assert!(
            db.get_reflection_by_id(&id).unwrap().is_none(),
            "soft-deleted reflection should not be visible"
        );

        // Soft deleting again returns false (already deleted).
        let deleted_again = db.soft_delete_reflection(&id).unwrap();
        assert!(
            !deleted_again,
            "soft_delete_reflection on already-deleted should return false"
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

    #[test]
    fn soft_delete_schedule_hides_from_queries() {
        let db = test_db();

        // Insert a schedule (using */Nm interval format).
        let id = db
            .insert_schedule("test-schedule", "*/30m", "echo test", "legion", None, None)
            .unwrap();

        // Verify it appears in list.
        let schedules = db.list_schedules().unwrap();
        assert!(
            schedules.iter().any(|s| s.id == id),
            "schedule should appear in list"
        );

        // Soft delete it.
        let deleted = db.soft_delete_schedule(&id).unwrap();
        assert!(deleted, "soft_delete_schedule should return true");

        // The schedule should now be invisible.
        let schedules_after = db.list_schedules().unwrap();
        assert!(
            !schedules_after.iter().any(|s| s.id == id),
            "soft-deleted schedule should not appear in list"
        );

        // Soft deleting again returns false.
        let deleted_again = db.soft_delete_schedule(&id).unwrap();
        assert!(
            !deleted_again,
            "soft_delete_schedule on already-deleted should return false"
        );
    }

    #[test]
    fn get_reflection_deltas_since_returns_modified_rows() {
        let db = test_db();

        // Insert two reflections.
        let r1 = db.insert_reflection("kelex", "first", "self").unwrap();
        let r2 = db.insert_reflection("kelex", "second", "self").unwrap();

        // Use a cutoff before both were created -- both should appear.
        let old_cutoff = "2020-01-01T00:00:00Z";
        let deltas = db.get_reflection_deltas_since(old_cutoff).unwrap();
        assert_eq!(deltas.len(), 2);

        // Use a cutoff after r1 but before r2 -- only r2 should appear.
        // (updated_at == created_at on insert, so r1.updated_at < r2.updated_at)
        let deltas_after_r1 = db
            .get_reflection_deltas_since(&r1.updated_at.unwrap())
            .unwrap();
        assert_eq!(deltas_after_r1.len(), 1);
        assert_eq!(deltas_after_r1[0].id, r2.id);

        // Boost r1 to bump its updated_at.
        std::thread::sleep(std::time::Duration::from_millis(10));
        db.boost_reflection(&r1.id).unwrap();
        let boosted = db.get_reflection_by_id(&r1.id).unwrap().unwrap();

        // Use r2's updated_at as cutoff -- now r1 should appear (it was boosted after).
        let deltas_after_r2 = db
            .get_reflection_deltas_since(&r2.updated_at.unwrap())
            .unwrap();
        assert_eq!(deltas_after_r2.len(), 1);
        assert_eq!(deltas_after_r2[0].id, r1.id);
        assert_eq!(deltas_after_r2[0].updated_at, boosted.updated_at);
    }

    #[test]
    fn get_reflection_deltas_since_includes_soft_deleted() {
        let db = test_db();

        let r = db
            .insert_reflection("kelex", "will delete", "self")
            .unwrap();
        let created_at = r.created_at.clone();

        // Soft delete the reflection.
        std::thread::sleep(std::time::Duration::from_millis(10));
        db.soft_delete_reflection(&r.id).unwrap();

        // Query with cutoff before creation -- should include the soft-deleted row.
        let deltas = db
            .get_reflection_deltas_since("2020-01-01T00:00:00Z")
            .unwrap();
        assert_eq!(deltas.len(), 1);
        assert_eq!(deltas[0].id, r.id);
        assert!(deltas[0].deleted_at.is_some(), "deleted_at should be set");

        // Query with cutoff after creation but before deletion -- should still include.
        let deltas_after_create = db.get_reflection_deltas_since(&created_at).unwrap();
        assert_eq!(deltas_after_create.len(), 1);
        assert!(deltas_after_create[0].deleted_at.is_some());
    }

    #[test]
    fn get_reflection_deltas_since_excludes_unchanged() {
        let db = test_db();

        let r = db.insert_reflection("kelex", "old", "self").unwrap();

        // Use a cutoff after the reflection was created -- should return empty.
        std::thread::sleep(std::time::Duration::from_millis(10));
        let future_cutoff = chrono::Utc::now().to_rfc3339();
        let deltas = db.get_reflection_deltas_since(&future_cutoff).unwrap();
        assert!(deltas.is_empty());

        // Verify the reflection still exists but wasn't returned.
        assert!(db.get_reflection_by_id(&r.id).unwrap().is_some());
    }

    #[test]
    fn get_card_deltas_since_returns_modified_cards() {
        let db = test_db();

        // Insert two cards.
        let id1 = db
            .insert_card(
                "kelex",
                "legion",
                "task 1",
                None,
                "med",
                None,
                None,
                None,
                None,
                None,
                crate::kanban::CardStatus::Pending,
            )
            .unwrap();
        let _id2 = db
            .insert_card(
                "kelex",
                "legion",
                "task 2",
                None,
                "high",
                None,
                None,
                None,
                None,
                None,
                crate::kanban::CardStatus::Pending,
            )
            .unwrap();

        // Use an old cutoff -- both should appear.
        let old_cutoff = "2020-01-01T00:00:00Z";
        let deltas = db.get_card_deltas_since(old_cutoff).unwrap();
        assert_eq!(deltas.len(), 2);

        // Verify fields are populated.
        let delta1 = deltas.iter().find(|d| d.id == id1).unwrap();
        assert_eq!(delta1.from_repo, "kelex");
        assert_eq!(delta1.to_repo, "legion");
        assert_eq!(delta1.text, "task 1");
        assert_eq!(delta1.priority, "med");
        assert_eq!(delta1.status, "pending");
        assert!(delta1.deleted_at.is_none());
    }

    #[test]
    fn get_card_deltas_since_includes_soft_deleted() {
        let db = test_db();

        let id = db
            .insert_card(
                "kelex",
                "legion",
                "will delete",
                None,
                "low",
                None,
                None,
                None,
                None,
                None,
                crate::kanban::CardStatus::Backlog,
            )
            .unwrap();

        // Soft delete the card.
        std::thread::sleep(std::time::Duration::from_millis(10));
        db.soft_delete_card(&id).unwrap();

        // Should still appear in deltas with deleted_at set.
        let deltas = db.get_card_deltas_since("2020-01-01T00:00:00Z").unwrap();
        assert_eq!(deltas.len(), 1);
        assert_eq!(deltas[0].id, id);
        assert!(deltas[0].deleted_at.is_some());
    }

    #[test]
    fn get_schedule_deltas_since_returns_modified_schedules() {
        let db = test_db();

        // Insert a schedule.
        let id = db
            .insert_schedule("test-sched", "*/30m", "echo hello", "legion", None, None)
            .unwrap();

        // Use an old cutoff -- should appear.
        let deltas = db
            .get_schedule_deltas_since("2020-01-01T00:00:00Z")
            .unwrap();
        assert_eq!(deltas.len(), 1);
        assert_eq!(deltas[0].id, id);
        assert_eq!(deltas[0].name, "test-sched");
        assert_eq!(deltas[0].cron, "*/30m");
        assert_eq!(deltas[0].command, "echo hello");
        assert!(deltas[0].enabled);
        assert!(deltas[0].deleted_at.is_none());
    }

    #[test]
    fn get_schedule_deltas_since_includes_soft_deleted() {
        let db = test_db();

        let id = db
            .insert_schedule("to-delete", "*/5m", "echo bye", "legion", None, None)
            .unwrap();

        // Soft delete.
        std::thread::sleep(std::time::Duration::from_millis(10));
        db.soft_delete_schedule(&id).unwrap();

        // Should appear with deleted_at set.
        let deltas = db
            .get_schedule_deltas_since("2020-01-01T00:00:00Z")
            .unwrap();
        assert_eq!(deltas.len(), 1);
        assert!(deltas[0].deleted_at.is_some());
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

    #[test]
    fn cleanup_tombstones_removes_old_soft_deleted_rows() {
        let db = test_db();

        // Insert and soft delete a reflection.
        let r = db.insert_reflection("kelex", "to delete", "self").unwrap();
        db.soft_delete_reflection(&r.id).unwrap();

        // Insert and soft delete a card.
        let card_id = db
            .insert_card(
                "kelex",
                "legion",
                "to delete",
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
        db.soft_delete_card(&card_id).unwrap();

        // Insert and soft delete a schedule.
        let sched_id = db
            .insert_schedule("to-delete", "*/5m", "echo bye", "legion", None, None)
            .unwrap();
        db.soft_delete_schedule(&sched_id).unwrap();

        // Cleanup with 0-day retention should remove all tombstones.
        let result = db.cleanup_tombstones(0).unwrap();
        assert_eq!(result.reflections, 1);
        assert_eq!(result.tasks, 1);
        assert_eq!(result.schedules, 1);
        assert_eq!(result.total(), 3);

        // Running again should return zeros.
        let result2 = db.cleanup_tombstones(0).unwrap();
        assert!(result2.is_empty());
    }

    #[test]
    fn cleanup_tombstones_respects_retention_period() {
        let db = test_db();

        // Insert and soft delete a reflection.
        let r = db
            .insert_reflection("kelex", "recent delete", "self")
            .unwrap();
        db.soft_delete_reflection(&r.id).unwrap();

        // Cleanup with 30-day retention should NOT remove the freshly deleted row.
        let result = db.cleanup_tombstones(30).unwrap();
        assert!(
            result.is_empty(),
            "fresh tombstone should not be cleaned up"
        );
    }

    fn rate_sample(
        id: &str,
        host: &str,
        sampled_at: &str,
        five_hour_pct: Option<f64>,
        seven_day_pct: Option<f64>,
    ) -> crate::statusline::RateLimitSample {
        crate::statusline::RateLimitSample {
            id: id.to_string(),
            hostname: host.to_string(),
            session_id: "sess".to_string(),
            sampled_at: sampled_at.to_string(),
            five_hour_pct,
            five_hour_resets_at: None,
            seven_day_pct,
            seven_day_resets_at: None,
            model: None,
        }
    }

    fn usage_sample(
        id: &str,
        host: &str,
        sampled_at: &str,
        effective: i64,
    ) -> crate::statusline::UsageSample {
        crate::statusline::UsageSample {
            id: id.to_string(),
            hostname: host.to_string(),
            session_id: "sess".to_string(),
            turn_index: None,
            model: None,
            input_tokens: 0,
            output_tokens: 0,
            cache_write_tokens: 0,
            cache_read_tokens: 0,
            effective_tokens: effective,
            error_bytes: 0,
            sampled_at: sampled_at.to_string(),
        }
    }

    #[test]
    fn latest_rate_limit_samples_per_host_returns_newest_per_host() {
        let db = test_db();
        // Two hosts, two samples each; expect the newest per host.
        db.insert_rate_limit_sample(&rate_sample(
            "1",
            "Puck",
            "2026-04-22T10:00:00Z",
            Some(30.0),
            Some(50.0),
        ))
        .unwrap();
        db.insert_rate_limit_sample(&rate_sample(
            "2",
            "Puck",
            "2026-04-22T11:00:00Z",
            Some(40.0),
            Some(55.0),
        ))
        .unwrap();
        db.insert_rate_limit_sample(&rate_sample(
            "3",
            "laptop",
            "2026-04-22T09:00:00Z",
            Some(60.0),
            Some(70.0),
        ))
        .unwrap();
        db.insert_rate_limit_sample(&rate_sample(
            "4",
            "laptop",
            "2026-04-22T12:00:00Z",
            Some(70.0),
            Some(75.0),
        ))
        .unwrap();

        let got = db.latest_rate_limit_samples_per_host().unwrap();
        assert_eq!(got.len(), 2);
        let by_host: std::collections::HashMap<_, _> =
            got.into_iter().map(|s| (s.hostname.clone(), s)).collect();
        assert_eq!(by_host["Puck"].id, "2");
        assert_eq!(by_host["laptop"].id, "4");
    }

    #[test]
    fn latest_rate_limit_samples_per_host_skips_soft_deleted() {
        let db = test_db();
        db.insert_rate_limit_sample(&rate_sample(
            "1",
            "Puck",
            "2026-04-22T10:00:00Z",
            Some(30.0),
            Some(50.0),
        ))
        .unwrap();
        db.insert_rate_limit_sample(&rate_sample(
            "2",
            "Puck",
            "2026-04-22T11:00:00Z",
            Some(40.0),
            Some(55.0),
        ))
        .unwrap();
        // Tombstone the newer row. The query should fall back to the older
        // row, NOT return Puck with sampled_at=null or skip the host entirely.
        db.conn
            .execute(
                "UPDATE rate_limit_samples SET deleted_at = ?1 WHERE id = '2'",
                rusqlite::params!["2026-04-22T12:00:00Z"],
            )
            .unwrap();

        let got = db.latest_rate_limit_samples_per_host().unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].id, "1");
    }

    #[test]
    fn latest_rate_limit_samples_per_host_empty_table() {
        let db = test_db();
        let got = db.latest_rate_limit_samples_per_host().unwrap();
        assert!(got.is_empty());
    }

    // -- Persona wake lease tests -------------------------------------------

    use std::time::Duration;

    #[test]
    fn persona_lease_acquire_succeeds_when_free() {
        let db = test_db();
        let got = db
            .try_acquire_persona_lease("legion", "sig-1", "hostA", Duration::from_secs(60))
            .unwrap();
        assert!(
            got,
            "first acquire on a free (persona, signal) must succeed"
        );
    }

    #[test]
    fn persona_lease_acquire_fails_when_held_by_another_host() {
        let db = test_db();
        assert!(
            db.try_acquire_persona_lease("legion", "sig-1", "hostA", Duration::from_secs(60))
                .unwrap()
        );
        let got = db
            .try_acquire_persona_lease("legion", "sig-1", "hostB", Duration::from_secs(60))
            .unwrap();
        assert!(
            !got,
            "second acquire on a live lease must report 'held' (false)"
        );

        let listed = db.list_persona_leases(Some("legion")).unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(
            listed[0].acquired_by_host, "hostA",
            "hostA's lease must remain untouched by hostB's failed acquire"
        );
    }

    #[test]
    fn persona_lease_acquire_succeeds_after_release() {
        let db = test_db();
        db.try_acquire_persona_lease("legion", "sig-1", "hostA", Duration::from_secs(60))
            .unwrap();
        let released = db.release_persona_lease("legion", "sig-1").unwrap();
        assert!(released, "release of a live lease must report true");

        let got = db
            .try_acquire_persona_lease("legion", "sig-1", "hostB", Duration::from_secs(60))
            .unwrap();
        assert!(got, "acquire after release must succeed");
    }

    #[test]
    fn persona_lease_release_is_idempotent() {
        let db = test_db();
        db.try_acquire_persona_lease("legion", "sig-1", "hostA", Duration::from_secs(60))
            .unwrap();
        assert!(db.release_persona_lease("legion", "sig-1").unwrap());
        assert!(
            !db.release_persona_lease("legion", "sig-1").unwrap(),
            "second release of the same lease must report false (already released)"
        );
    }

    #[test]
    fn persona_lease_acquire_succeeds_after_expiry() {
        let db = test_db();
        // TTL of 0 seconds -> lease expires immediately.
        assert!(
            db.try_acquire_persona_lease("legion", "sig-1", "hostA", Duration::from_secs(0))
                .unwrap()
        );
        // Sleep long enough that the clock advances past `expires_at`.
        std::thread::sleep(Duration::from_millis(10));
        let got = db
            .try_acquire_persona_lease("legion", "sig-1", "hostB", Duration::from_secs(60))
            .unwrap();
        assert!(
            got,
            "acquire against an expired lease must succeed (hostB takes over)"
        );

        let listed = db.list_persona_leases(Some("legion")).unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(
            listed[0].acquired_by_host, "hostB",
            "the fresh lease must be owned by the reacquirer"
        );
    }

    #[test]
    fn persona_lease_heartbeat_extends_expiry() {
        let db = test_db();
        db.try_acquire_persona_lease("legion", "sig-1", "hostA", Duration::from_secs(60))
            .unwrap();
        let before = db.list_persona_leases(Some("legion")).unwrap().remove(0);

        std::thread::sleep(Duration::from_millis(20));
        let n = db
            .heartbeat_persona_leases("hostA", Duration::from_secs(3600))
            .unwrap();
        assert_eq!(n, 1, "heartbeat should touch exactly hostA's live lease");

        let after = db.list_persona_leases(Some("legion")).unwrap().remove(0);
        assert!(
            after.expires_at > before.expires_at,
            "heartbeat must push expires_at forward (before: {}, after: {})",
            before.expires_at,
            after.expires_at
        );
        assert!(
            after.heartbeat_at > before.heartbeat_at,
            "heartbeat must advance heartbeat_at"
        );
    }

    #[test]
    fn persona_lease_heartbeat_skips_foreign_hosts() {
        let db = test_db();
        db.try_acquire_persona_lease("legion", "sig-1", "hostA", Duration::from_secs(60))
            .unwrap();
        let n = db
            .heartbeat_persona_leases("hostB", Duration::from_secs(3600))
            .unwrap();
        assert_eq!(n, 0, "heartbeat must only touch the caller's leases");
    }

    #[test]
    fn persona_lease_list_filters_by_persona() {
        let db = test_db();
        db.try_acquire_persona_lease("legion", "sig-1", "hostA", Duration::from_secs(60))
            .unwrap();
        db.try_acquire_persona_lease("huttspawn", "sig-2", "hostA", Duration::from_secs(60))
            .unwrap();

        let all = db.list_persona_leases(None).unwrap();
        assert_eq!(all.len(), 2);

        let legion_only = db.list_persona_leases(Some("legion")).unwrap();
        assert_eq!(legion_only.len(), 1);
        assert_eq!(legion_only[0].persona_id, "legion");
    }

    #[test]
    fn persona_lease_list_omits_expired() {
        let db = test_db();
        db.try_acquire_persona_lease("legion", "sig-1", "hostA", Duration::from_secs(0))
            .unwrap();
        std::thread::sleep(Duration::from_millis(10));
        let listed = db.list_persona_leases(None).unwrap();
        assert!(listed.is_empty(), "expired leases must not appear in list");
    }

    #[test]
    fn persona_lease_list_omits_tombstones() {
        let db = test_db();
        db.try_acquire_persona_lease("legion", "sig-1", "hostA", Duration::from_secs(60))
            .unwrap();
        db.release_persona_lease("legion", "sig-1").unwrap();
        let listed = db.list_persona_leases(None).unwrap();
        assert!(listed.is_empty(), "released leases must not appear in list");
    }

    #[test]
    fn persona_lease_release_by_host_clears_all_host_leases() {
        let db = test_db();
        db.try_acquire_persona_lease("legion", "sig-1", "hostA", Duration::from_secs(60))
            .unwrap();
        db.try_acquire_persona_lease("huttspawn", "sig-2", "hostA", Duration::from_secs(60))
            .unwrap();
        db.try_acquire_persona_lease("kessel", "sig-3", "hostB", Duration::from_secs(60))
            .unwrap();

        let cleared = db.release_persona_leases_by_host("hostA").unwrap();
        assert_eq!(cleared, 2, "must release exactly hostA's two leases");

        let remaining = db.list_persona_leases(None).unwrap();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].acquired_by_host, "hostB");
    }

    #[test]
    fn persona_lease_apply_delta_inserts_new() {
        let db = test_db();
        let delta = crate::sync::PersonaWakeLeaseDelta {
            persona_id: "legion".into(),
            signal_id: "sig-1".into(),
            acquired_by_host: "peer".into(),
            acquired_at: "2026-04-24T00:00:00Z".into(),
            heartbeat_at: "2026-04-24T00:00:00Z".into(),
            expires_at: "2099-01-01T00:00:00Z".into(),
            updated_at: "2026-04-24T00:00:00Z".into(),
            deleted_at: None,
        };
        let late = db.apply_persona_wake_lease_delta(&delta).unwrap();
        assert!(late.is_none(), "no local row means no late loser");
        let listed = db.list_persona_leases(None).unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].acquired_by_host, "peer");
    }

    #[test]
    fn persona_lease_apply_delta_earlier_acquired_at_wins() {
        // Two real acquires against separate databases, 50ms apart, then
        // sync-apply the earlier one onto the later's database and assert
        // the earlier wins. Uses realistic clock deltas rather than hardcoded
        // ancient timestamps so the test exercises actual RFC3339 ordering
        // at sub-second precision.
        let peer_db = test_db();
        assert!(
            peer_db
                .try_acquire_persona_lease("legion", "sig-1", "peer", Duration::from_secs(3600))
                .unwrap()
        );
        let peer_row = peer_db.list_persona_leases(None).unwrap().remove(0);

        std::thread::sleep(Duration::from_millis(50));

        let local_db = test_db();
        assert!(
            local_db
                .try_acquire_persona_lease("legion", "sig-1", "local", Duration::from_secs(3600))
                .unwrap()
        );

        // Peer's lease is older; when its delta reaches local, local is the
        // late loser.
        let delta = crate::sync::PersonaWakeLeaseDelta {
            persona_id: peer_row.persona_id,
            signal_id: peer_row.signal_id,
            acquired_by_host: peer_row.acquired_by_host,
            acquired_at: peer_row.acquired_at.clone(),
            heartbeat_at: peer_row.heartbeat_at,
            expires_at: peer_row.expires_at,
            updated_at: peer_row.updated_at,
            deleted_at: peer_row.deleted_at,
        };
        let late = local_db.apply_persona_wake_lease_delta(&delta).unwrap();
        assert_eq!(
            late.as_deref(),
            Some("local"),
            "local node is the late loser; its host identity must surface"
        );

        let listed = local_db.list_persona_leases(None).unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(
            listed[0].acquired_by_host, "peer",
            "peer's earlier lease must win"
        );
        assert_eq!(
            listed[0].acquired_at, delta.acquired_at,
            "winning acquired_at must be peer's, not local's"
        );
    }

    #[test]
    fn persona_lease_acquire_succeeds_after_ttl_expires_without_release() {
        // Crash-recovery path: the holder acquires with a short TTL, never
        // calls release (simulating a crash), and after the TTL elapses the
        // next acquirer succeeds. This is the behavior the issue calls out:
        // "session crashes without releasing -> lease expires via heartbeat
        // TTL. Another wake on the same signal succeeds after expiration."
        let db = test_db();
        assert!(
            db.try_acquire_persona_lease(
                "legion",
                "sig-1",
                "crashy-host",
                Duration::from_millis(100)
            )
            .unwrap()
        );

        // While the lease is still live, a second acquire must fail.
        assert!(
            !db.try_acquire_persona_lease(
                "legion",
                "sig-1",
                "recovery-host",
                Duration::from_secs(3600)
            )
            .unwrap(),
            "live lease (even near expiry) must block a concurrent acquire"
        );

        // Wait past the TTL without calling release.
        std::thread::sleep(Duration::from_millis(200));

        assert!(
            db.try_acquire_persona_lease(
                "legion",
                "sig-1",
                "recovery-host",
                Duration::from_secs(3600)
            )
            .unwrap(),
            "after TTL elapses, a new acquirer must succeed"
        );

        let listed = db.list_persona_leases(None).unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(
            listed[0].acquired_by_host, "recovery-host",
            "recovery host must own the post-crash lease"
        );
    }

    #[test]
    fn persona_lease_acquire_is_cross_connection_race_safe() {
        // Issue #308 atomicity contract: two independent Database handles
        // against the same file race to acquire the same (persona, signal).
        // Each thread opens its own handle (Database is !Send because it
        // wraps rusqlite::Connection; ownership stays thread-local). Exactly
        // one must win; neither can surface SQLITE_BUSY as Err.
        use std::thread;

        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("race.sqlite");

        // Prime the schema once so neither racing thread takes the migration
        // path (what's being tested is acquire atomicity, not open atomicity).
        let _ = Database::open(&db_path).unwrap();

        let path_a = db_path.clone();
        let path_b = db_path.clone();

        let t_a = thread::spawn(move || -> Result<bool> {
            let db = Database::open(&path_a)?;
            db.try_acquire_persona_lease("legion", "sig-race", "host-A", Duration::from_secs(60))
        });
        let t_b = thread::spawn(move || -> Result<bool> {
            let db = Database::open(&path_b)?;
            db.try_acquire_persona_lease("legion", "sig-race", "host-B", Duration::from_secs(60))
        });

        let r_a = t_a.join().unwrap();
        let r_b = t_b.join().unwrap();

        let mut wins = 0usize;
        for r in [&r_a, &r_b] {
            match r {
                Ok(true) => wins += 1,
                Ok(false) => {}
                Err(e) => panic!("acquire surfaced SQLITE_BUSY as Err: {e}"),
            }
        }
        assert_eq!(
            wins, 1,
            "exactly one concurrent acquire must win (got {} winners)",
            wins
        );

        let observer = Database::open(&db_path).unwrap();
        let listed = observer.list_persona_leases(None).unwrap();
        assert_eq!(listed.len(), 1);
        assert!(
            listed[0].acquired_by_host == "host-A" || listed[0].acquired_by_host == "host-B",
            "unexpected host recorded: {}",
            listed[0].acquired_by_host
        );
    }

    #[test]
    fn persona_lease_release_if_owner_refuses_foreign_host() {
        // Guards the late-loser reaper scenario: after sync conflict
        // resolution overwrites local's row with peer's, local's AgentTracker
        // will try to reap and release the lease it thought it held. The
        // host-scoped release must refuse because the row now belongs to
        // peer, preventing the late-loser from dropping the winner's lease.
        let db = test_db();
        db.try_acquire_persona_lease("legion", "sig-1", "peer", Duration::from_secs(60))
            .unwrap();

        let released = db
            .release_persona_lease_if_owner("legion", "sig-1", "late-loser")
            .unwrap();
        assert!(
            !released,
            "host-scoped release must refuse to touch a row owned by another host"
        );

        let listed = db.list_persona_leases(None).unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(
            listed[0].acquired_by_host, "peer",
            "peer's lease must survive the late-loser's release attempt"
        );
    }

    #[test]
    fn persona_lease_apply_delta_tombstone_wins_by_lww() {
        let db = test_db();
        db.try_acquire_persona_lease("legion", "sig-1", "local", Duration::from_secs(60))
            .unwrap();

        // Incoming tombstone with a later updated_at.
        let delta = crate::sync::PersonaWakeLeaseDelta {
            persona_id: "legion".into(),
            signal_id: "sig-1".into(),
            acquired_by_host: "local".into(),
            acquired_at: "2026-04-24T00:00:00Z".into(),
            heartbeat_at: "2026-04-24T00:00:00Z".into(),
            expires_at: "2099-01-01T00:00:00Z".into(),
            updated_at: "2099-01-01T00:00:00Z".into(),
            deleted_at: Some("2099-01-01T00:00:00Z".into()),
        };
        db.apply_persona_wake_lease_delta(&delta).unwrap();

        let listed = db.list_persona_leases(None).unwrap();
        assert!(
            listed.is_empty(),
            "incoming tombstone with newer updated_at must eclipse local live lease"
        );
    }

    // -- apply_*_delta LWW (#536) ------------------------------------------

    fn reflection_delta(id: &str, text: &str, updated_at: &str) -> crate::sync::ReflectionDelta {
        crate::sync::ReflectionDelta {
            id: id.into(),
            repo: "legion".into(),
            text: text.into(),
            created_at: "2026-06-01T00:00:00Z".into(),
            updated_at: Some(updated_at.into()),
            deleted_at: None,
            audience: "self".into(),
            domain: None,
            tags: None,
            recall_count: 0,
            last_recalled_at: None,
            parent_id: None,
        }
    }

    fn read_reflection_text(db: &Database, id: &str) -> Option<String> {
        db.conn
            .query_row("SELECT text FROM reflections WHERE id = ?1", [id], |r| {
                r.get(0)
            })
            .optional()
            .unwrap()
    }

    #[test]
    fn apply_reflection_delta_inserts_when_missing() {
        let db = test_db();
        db.apply_reflection_delta(&reflection_delta(
            "r-1",
            "from peer",
            "2026-06-02T00:00:00Z",
        ))
        .unwrap();
        assert_eq!(
            read_reflection_text(&db, "r-1").as_deref(),
            Some("from peer")
        );
    }

    #[test]
    fn apply_reflection_delta_newer_overwrites_older_is_noop() {
        let db = test_db();
        db.apply_reflection_delta(&reflection_delta("r-2", "v1", "2026-06-02T00:00:00Z"))
            .unwrap();
        // Newer delta wins.
        db.apply_reflection_delta(&reflection_delta("r-2", "v2", "2026-06-03T00:00:00Z"))
            .unwrap();
        assert_eq!(read_reflection_text(&db, "r-2").as_deref(), Some("v2"));
        // Older delta is a no-op.
        db.apply_reflection_delta(&reflection_delta("r-2", "stale", "2026-06-01T00:00:00Z"))
            .unwrap();
        assert_eq!(read_reflection_text(&db, "r-2").as_deref(), Some("v2"));
    }

    #[test]
    fn apply_reflection_delta_tombstone_wins_by_lww() {
        let db = test_db();
        db.apply_reflection_delta(&reflection_delta("r-3", "alive", "2026-06-02T00:00:00Z"))
            .unwrap();
        let mut tomb = reflection_delta("r-3", "alive", "2026-06-02T00:00:00Z");
        tomb.deleted_at = Some("2026-06-04T00:00:00Z".into());
        db.apply_reflection_delta(&tomb).unwrap();
        let deleted: Option<String> = db
            .conn
            .query_row(
                "SELECT deleted_at FROM reflections WHERE id = ?1",
                ["r-3"],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(deleted.as_deref(), Some("2026-06-04T00:00:00Z"));
    }

    #[test]
    fn apply_card_delta_insert_then_lww() {
        let db = test_db();
        let mut delta = crate::sync::CardDelta {
            id: "c-1".into(),
            from_repo: "legion".into(),
            to_repo: "legion".into(),
            text: "card v1".into(),
            context: None,
            priority: "med".into(),
            status: "pending".into(),
            note: None,
            labels: None,
            parent_card_id: None,
            source_url: None,
            source_type: None,
            sort_order: 0,
            created_at: "2026-06-01T00:00:00Z".into(),
            updated_at: "2026-06-02T00:00:00Z".into(),
            deleted_at: None,
            assigned_at: None,
            started_at: None,
            completed_at: None,
            problem: None,
            solution: None,
            acceptance: None,
        };
        db.apply_card_delta(&delta).unwrap();

        delta.text = "card v2".into();
        delta.updated_at = "2026-06-03T00:00:00Z".into();
        db.apply_card_delta(&delta).unwrap();

        // Stale write loses.
        delta.text = "card stale".into();
        delta.updated_at = "2026-06-01T12:00:00Z".into();
        db.apply_card_delta(&delta).unwrap();

        let text: String = db
            .conn
            .query_row("SELECT text FROM tasks WHERE id = ?1", ["c-1"], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(text, "card v2");
    }

    #[test]
    fn apply_schedule_delta_insert_then_lww() {
        let db = test_db();
        let mut delta = crate::sync::ScheduleDelta {
            id: "s-1".into(),
            name: "nightly".into(),
            cron: "0 2 * * *".into(),
            command: "echo one".into(),
            repo: "legion".into(),
            enabled: true,
            last_run: None,
            next_run: "2026-06-02T02:00:00Z".into(),
            created_at: "2026-06-01T00:00:00Z".into(),
            updated_at: Some("2026-06-02T00:00:00Z".into()),
            deleted_at: None,
            active_start: None,
            active_end: None,
        };
        db.apply_schedule_delta(&delta).unwrap();

        delta.command = "echo two".into();
        delta.updated_at = Some("2026-06-03T00:00:00Z".into());
        db.apply_schedule_delta(&delta).unwrap();

        delta.command = "echo stale".into();
        delta.updated_at = Some("2026-06-01T06:00:00Z".into());
        db.apply_schedule_delta(&delta).unwrap();

        let cmd: String = db
            .conn
            .query_row(
                "SELECT command FROM schedules WHERE id = ?1",
                ["s-1"],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(cmd, "echo two");
    }

    #[test]
    fn effective_sync_ts_picks_latest_of_three() {
        assert_eq!(
            effective_sync_ts("2026-01-01T00:00:00Z", &None, &None),
            "2026-01-01T00:00:00Z"
        );
        assert_eq!(
            effective_sync_ts(
                "2026-01-01T00:00:00Z",
                &Some("2026-02-01T00:00:00Z".into()),
                &None
            ),
            "2026-02-01T00:00:00Z"
        );
        assert_eq!(
            effective_sync_ts(
                "2026-01-01T00:00:00Z",
                &Some("2026-02-01T00:00:00Z".into()),
                &Some("2026-03-01T00:00:00Z".into())
            ),
            "2026-03-01T00:00:00Z"
        );
    }

    #[test]
    fn latest_usage_samples_per_host_returns_newest_per_host() {
        let db = test_db();
        db.insert_usage_sample(&usage_sample("1", "Puck", "2026-04-22T10:00:00Z", 100))
            .unwrap();
        db.insert_usage_sample(&usage_sample("2", "Puck", "2026-04-22T11:00:00Z", 200))
            .unwrap();
        db.insert_usage_sample(&usage_sample("3", "laptop", "2026-04-22T11:30:00Z", 300))
            .unwrap();

        let got = db.latest_usage_samples_per_host().unwrap();
        assert_eq!(got.len(), 2);
        let by_host: std::collections::HashMap<_, _> =
            got.into_iter().map(|s| (s.hostname.clone(), s)).collect();
        assert_eq!(by_host["Puck"].effective_tokens, 200);
        assert_eq!(by_host["laptop"].effective_tokens, 300);
    }

    fn make_scip(repo: &str, lang: &str, blob: &[u8]) -> crate::scip::ScipIndex {
        crate::scip::ScipIndex {
            id: uuid::Uuid::now_v7().to_string(),
            repo: repo.to_string(),
            lang: lang.to_string(),
            content_hash: crate::scip::content_hash(blob),
            blob: blob.to_vec(),
            updated_at: "2026-04-28T00:00:00Z".to_string(),
            deleted_at: None,
        }
    }

    #[test]
    fn scip_upsert_then_get_round_trip() {
        let db = test_db();
        let idx = make_scip("legion", "rust", b"hello scip");
        db.upsert_scip_index(&idx).unwrap();

        let got = db.get_scip_index("legion", "rust").unwrap().unwrap();
        assert_eq!(got.content_hash, idx.content_hash);
        assert_eq!(got.blob, idx.blob);
        assert_eq!(got.lang, "rust");
    }

    #[test]
    fn scip_upsert_idempotent_on_unchanged_hash() {
        let db = test_db();
        let mut idx = make_scip("legion", "rust", b"same bytes");
        db.upsert_scip_index(&idx).unwrap();
        let original_id = db.get_scip_index("legion", "rust").unwrap().unwrap().id;

        // Second call with identical content_hash -- bumps updated_at
        // but does not rewrite the row identity.
        idx.updated_at = "2026-04-29T00:00:00Z".to_string();
        db.upsert_scip_index(&idx).unwrap();

        let after = db.get_scip_index("legion", "rust").unwrap().unwrap();
        assert_eq!(after.id, original_id, "upsert must not replace the row");
        assert_eq!(after.updated_at, "2026-04-29T00:00:00Z");
    }

    #[test]
    fn scip_upsert_overwrites_blob_when_hash_changes() {
        let db = test_db();
        db.upsert_scip_index(&make_scip("legion", "rust", b"v1"))
            .unwrap();
        db.upsert_scip_index(&make_scip("legion", "rust", b"v2"))
            .unwrap();

        let got = db.get_scip_index("legion", "rust").unwrap().unwrap();
        assert_eq!(got.blob, b"v2");
    }

    #[test]
    fn scip_list_returns_all_languages_for_repo() {
        let db = test_db();
        db.upsert_scip_index(&make_scip("legion", "rust", b"r"))
            .unwrap();
        db.upsert_scip_index(&make_scip("legion", "ts", b"t"))
            .unwrap();
        db.upsert_scip_index(&make_scip("other", "rust", b"x"))
            .unwrap();

        let mut langs: Vec<String> = db
            .list_scip_indexes("legion")
            .unwrap()
            .into_iter()
            .map(|i| i.lang)
            .collect();
        langs.sort();
        assert_eq!(langs, vec!["rust".to_string(), "ts".to_string()]);
    }

    #[test]
    fn scip_soft_deleted_row_excluded_from_list() {
        let db = test_db();
        let idx = make_scip("legion", "rust", b"x");
        db.upsert_scip_index(&idx).unwrap();
        db.conn
            .execute(
                "UPDATE scip_indexes SET deleted_at = '2026-04-28T00:00:00Z' WHERE repo = 'legion'",
                [],
            )
            .unwrap();

        assert!(
            db.list_scip_indexes("legion").unwrap().is_empty(),
            "soft-deleted rows must not appear in list"
        );
        assert!(
            db.get_scip_index("legion", "rust").unwrap().is_none(),
            "soft-deleted rows must not appear in get"
        );

        // Raw row still present in the table.
        let count: i64 = db
            .conn
            .query_row("SELECT COUNT(*) FROM scip_indexes", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 1);
    }

    // -- Bullpen decay (#376) --------------------------------------------

    fn t_now() -> chrono::DateTime<Utc> {
        Utc::now()
    }

    #[test]
    fn ttl_default_post_is_48_hours() {
        let (e, ev) = compute_post_ttl("team", None, None, "broadcast", t_now());
        assert!(!ev);
        let exp = chrono::DateTime::parse_from_rfc3339(&e.expect("expires_at")).unwrap();
        let delta = exp.signed_duration_since(t_now()).num_hours();
        assert!(
            (47..=48).contains(&delta),
            "default TTL must be 48h, got {delta}"
        );
    }

    #[test]
    fn ttl_signal_post_is_7_days() {
        let (e, _) = compute_post_ttl("team", None, None, "@smugglr review:requested", t_now());
        let exp = chrono::DateTime::parse_from_rfc3339(&e.expect("expires_at")).unwrap();
        let hours = exp.signed_duration_since(t_now()).num_hours();
        assert!(
            (167..=168).contains(&hours),
            "signal TTL must be 7d (~168h), got {hours}"
        );
    }

    #[test]
    fn ttl_design_tag_is_14_days() {
        let (e, _) = compute_post_ttl(
            "team",
            None,
            Some("design,blueprint"),
            "design proposal",
            t_now(),
        );
        let exp = chrono::DateTime::parse_from_rfc3339(&e.expect("expires_at")).unwrap();
        let hours = exp.signed_duration_since(t_now()).num_hours();
        assert!(
            (335..=336).contains(&hours),
            "design TTL must be 14d (~336h), got {hours}"
        );
    }

    #[test]
    fn ttl_identity_domain_is_evergreen() {
        let (e, ev) = compute_post_ttl("team", Some("identity"), None, "who I am", t_now());
        assert_eq!(e, None, "evergreen rows have no expires_at");
        assert!(ev);
    }

    #[test]
    fn ttl_doctrine_tag_is_evergreen() {
        let (e, ev) = compute_post_ttl("team", None, Some("doctrine"), "doctrine post", t_now());
        assert_eq!(e, None);
        assert!(ev);
    }

    #[test]
    fn ttl_non_team_audience_is_skipped() {
        let (e, ev) = compute_post_ttl("self", None, None, "private", t_now());
        assert_eq!(e, None);
        assert!(!ev);
    }

    #[test]
    fn get_board_posts_filters_expired() {
        let db = test_db();
        // Fresh team post -- visible.
        db.insert_reflection("kelex", "fresh broadcast", "team")
            .unwrap();
        // Manually-aged team post (created 10 days ago, expires 48h after that).
        let aged = "2026-04-15T00:00:00+00:00";
        let aged_exp = "2026-04-17T00:00:00+00:00";
        db.conn
            .execute(
                "INSERT INTO reflections (id, repo, text, created_at, audience, expires_at, evergreen) \
                 VALUES (?1, 'kelex', 'stale post', ?2, 'team', ?3, 0)",
                rusqlite::params![Uuid::now_v7().to_string(), aged, aged_exp],
            )
            .unwrap();

        let posts = db.get_board_posts().unwrap();
        assert_eq!(posts.len(), 1);
        assert_eq!(posts[0].text, "fresh broadcast");
    }

    #[test]
    fn get_board_posts_include_stale_returns_expired() {
        let db = test_db();
        let aged = "2026-04-15T00:00:00+00:00";
        let aged_exp = "2026-04-17T00:00:00+00:00";
        db.conn
            .execute(
                "INSERT INTO reflections (id, repo, text, created_at, audience, expires_at, evergreen) \
                 VALUES (?1, 'kelex', 'stale post', ?2, 'team', ?3, 0)",
                rusqlite::params![Uuid::now_v7().to_string(), aged, aged_exp],
            )
            .unwrap();
        let active = db.get_board_posts_filtered(false).unwrap();
        assert!(active.is_empty());
        let all = db.get_board_posts_filtered(true).unwrap();
        assert_eq!(all.len(), 1);
    }

    #[test]
    fn get_board_posts_keeps_evergreen() {
        let db = test_db();
        let aged = "2026-01-01T00:00:00+00:00";
        db.conn
            .execute(
                "INSERT INTO reflections (id, repo, text, created_at, audience, expires_at, evergreen) \
                 VALUES (?1, 'kelex', 'doctrine post', ?2, 'team', NULL, 1)",
                rusqlite::params![Uuid::now_v7().to_string(), aged],
            )
            .unwrap();
        let posts = db.get_board_posts().unwrap();
        assert_eq!(posts.len(), 1);
        assert_eq!(posts[0].text, "doctrine post");
    }

    #[test]
    fn get_board_posts_since_filters_expired() {
        let db = test_db();
        let aged = "2026-04-15T00:00:00+00:00";
        let aged_exp = "2026-04-17T00:00:00+00:00";
        db.conn
            .execute(
                "INSERT INTO reflections (id, repo, text, created_at, audience, expires_at, evergreen) \
                 VALUES (?1, 'kelex', 'stale signal', ?2, 'team', ?3, 0)",
                rusqlite::params![Uuid::now_v7().to_string(), aged, aged_exp],
            )
            .unwrap();
        // Cursor before the aged row -- with decay filter, batch must be empty.
        let batch = db
            .get_board_posts_since("2026-01-01T00:00:00+00:00", "", 100)
            .unwrap();
        assert!(batch.is_empty(), "stale post must not push notifications");
    }

    #[test]
    fn get_unhandled_signals_filters_expired() {
        let db = test_db();
        let aged = "2026-04-15T00:00:00+00:00";
        let aged_exp = "2026-04-17T00:00:00+00:00";
        db.conn
            .execute(
                "INSERT INTO reflections (id, repo, text, created_at, audience, expires_at, evergreen) \
                 VALUES (?1, 'platform', '@kessel reply please ', ?2, 'team', ?3, 0)",
                rusqlite::params![Uuid::now_v7().to_string(), aged, aged_exp],
            )
            .unwrap();
        let signals = db
            .get_unhandled_signals_for_repo("kessel", &["kessel".to_string()], None)
            .unwrap();
        assert!(
            signals.is_empty(),
            "wake loop must not deliver stale @kessel signals"
        );
    }

    #[test]
    fn insert_team_post_sets_decay_metadata() {
        let db = test_db();
        let r = db
            .insert_reflection("kelex", "fresh broadcast", "team")
            .unwrap();
        let (expires_at, evergreen): (Option<String>, i32) = db
            .conn
            .query_row(
                "SELECT expires_at, evergreen FROM reflections WHERE id = ?1",
                [&r.id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert!(expires_at.is_some(), "team post must get expires_at");
        assert_eq!(evergreen, 0);
    }

    // -- Resolution marker (#362) ----------------------------------------

    #[test]
    fn resolve_post_marks_row_resolved() {
        let db = test_db();
        let r = db
            .insert_reflection("kelex", "fresh broadcast", "team")
            .unwrap();
        let updated = db.resolve_post(&r.id, None).unwrap();
        assert!(updated);

        let resolved_at: Option<String> = db
            .conn
            .query_row(
                "SELECT resolved_at FROM reflections WHERE id = ?1",
                [&r.id],
                |row| row.get(0),
            )
            .unwrap();
        assert!(resolved_at.is_some(), "resolved_at must be populated");
    }

    #[test]
    fn resolve_post_links_reflection_id() {
        let db = test_db();
        let r = db
            .insert_reflection("kelex", "fresh broadcast", "team")
            .unwrap();
        let conclusion = db
            .insert_reflection("kelex", "team agreed on X", "self")
            .unwrap();
        db.resolve_post(&r.id, Some(&conclusion.id)).unwrap();

        let linked: Option<String> = db
            .conn
            .query_row(
                "SELECT resolved_by_reflection_id FROM reflections WHERE id = ?1",
                [&r.id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(linked, Some(conclusion.id));
    }

    #[test]
    fn resolve_post_returns_false_for_missing_id() {
        let db = test_db();
        let updated = db
            .resolve_post("00000000-0000-0000-0000-000000000000", None)
            .unwrap();
        assert!(!updated);
    }

    #[test]
    fn get_board_posts_filters_resolved() {
        let db = test_db();
        let kept = db
            .insert_reflection("kelex", "open thread", "team")
            .unwrap();
        let r = db
            .insert_reflection("kelex", "converged thread", "team")
            .unwrap();
        db.resolve_post(&r.id, None).unwrap();

        let posts = db.get_board_posts().unwrap();
        assert_eq!(posts.len(), 1);
        assert_eq!(posts[0].id, kept.id);
    }

    #[test]
    fn get_board_posts_include_resolved_returns_resolved() {
        let db = test_db();
        let r = db
            .insert_reflection("kelex", "converged thread", "team")
            .unwrap();
        db.resolve_post(&r.id, None).unwrap();
        let active = db.get_board_posts_filtered_full(false, false).unwrap();
        assert!(active.is_empty());
        let with_resolved = db.get_board_posts_filtered_full(false, true).unwrap();
        assert_eq!(with_resolved.len(), 1);
    }

    #[test]
    fn get_board_posts_since_skips_resolved() {
        let db = test_db();
        let r = db
            .insert_reflection("kelex", "converged thread", "team")
            .unwrap();
        db.resolve_post(&r.id, None).unwrap();
        let batch = db
            .get_board_posts_since("2026-01-01T00:00:00+00:00", "", 100)
            .unwrap();
        assert!(batch.is_empty(), "channel must not push resolved posts");
    }

    #[test]
    fn get_unhandled_signals_skips_resolved() {
        let db = test_db();
        let r = db
            .insert_reflection("platform", "@kessel needs answer ", "team")
            .unwrap();
        db.resolve_post(&r.id, None).unwrap();
        let signals = db
            .get_unhandled_signals_for_repo("kessel", &["kessel".to_string()], None)
            .unwrap();
        assert!(
            signals.is_empty(),
            "wake loop must not deliver resolved signals"
        );
    }

    #[test]
    fn get_unread_count_skips_resolved() {
        let db = test_db();
        let r = db.insert_reflection("kelex", "converged", "team").unwrap();
        db.resolve_post(&r.id, None).unwrap();
        let count = db.get_unread_count("smugglr").unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn insert_self_reflection_skips_decay() {
        let db = test_db();
        let r = db
            .insert_reflection("kelex", "private note", "self")
            .unwrap();
        let (expires_at, evergreen): (Option<String>, i32) = db
            .conn
            .query_row(
                "SELECT expires_at, evergreen FROM reflections WHERE id = ?1",
                [&r.id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(expires_at, None, "self audience never expires");
        assert_eq!(evergreen, 0);
    }

    // -- Agent session log (#389) ---------------------------------------

    fn within_window() -> (String, String) {
        let now = Utc::now();
        (
            (now - chrono::Duration::seconds(30)).to_rfc3339(),
            (now + chrono::Duration::seconds(30)).to_rfc3339(),
        )
    }

    #[test]
    fn classify_session_productive_via_bullpen_reply() {
        let db = test_db();
        let (spawn_at, exit_at) = within_window();
        // Recipient posts to bullpen during the session window.
        db.insert_reflection("shingle", "@huttspawn ack:done -- reviewed", "team")
            .unwrap();
        let productive = db
            .classify_session("shingle", &["sig-1".to_string()], &spawn_at, &exit_at)
            .unwrap();
        assert!(
            productive,
            "bullpen reply within window must classify productive"
        );
    }

    #[test]
    fn classify_session_productive_via_reflection_link() {
        let db = test_db();
        let (spawn_at, exit_at) = within_window();
        // Signal post owned by some other repo; recipient links to it.
        let signal = db
            .insert_reflection("huttspawn", "@shingle review:open", "team")
            .unwrap();
        let meta = ReflectionMeta {
            domain: None,
            tags: None,
            parent_id: Some(signal.id.clone()),
        };
        db.insert_reflection_with_meta("shingle", "thinking through review", "self", &meta)
            .unwrap();
        let productive = db
            .classify_session("shingle", &[signal.id], &spawn_at, &exit_at)
            .unwrap();
        assert!(
            productive,
            "linked reflection within window must classify productive"
        );
    }

    #[test]
    fn classify_session_unproductive_when_silent() {
        let db = test_db();
        let (spawn_at, exit_at) = within_window();
        let signal = db
            .insert_reflection("huttspawn", "@shingle review:open", "team")
            .unwrap();
        let productive = db
            .classify_session("shingle", &[signal.id], &spawn_at, &exit_at)
            .unwrap();
        assert!(
            !productive,
            "no bullpen reply, no linked reflection -> unproductive"
        );
    }

    #[test]
    fn classify_session_ignores_outside_window() {
        let db = test_db();
        // Spawn/exit window in the past; recipient posts now (after window).
        let spawn_at = "2026-01-01T00:00:00+00:00".to_string();
        let exit_at = "2026-01-01T00:01:00+00:00".to_string();
        db.insert_reflection("shingle", "@huttspawn ack:done", "team")
            .unwrap();
        let productive = db
            .classify_session("shingle", &["sig-1".to_string()], &spawn_at, &exit_at)
            .unwrap();
        assert!(
            !productive,
            "post outside the spawn-exit window must not count"
        );
    }

    #[test]
    fn record_session_outcome_persists_row() {
        let db = test_db();
        let (spawn_at, exit_at) = within_window();
        db.record_session_outcome(
            "session-1",
            "shingle",
            &["sig-1".to_string(), "sig-2".to_string()],
            &spawn_at,
            &exit_at,
            "ok",
            Database::OUTCOME_UNPRODUCTIVE,
        )
        .unwrap();
        let counts = db
            .agent_session_counts_since("2026-01-01T00:00:00+00:00")
            .unwrap();
        let row = counts.iter().find(|r| r.0 == "shingle").expect("row");
        assert_eq!(row.1, 1, "total");
        assert_eq!(row.2, 0, "productive");
        assert_eq!(row.3, 1, "unproductive");
        assert_eq!(row.5.as_deref(), Some("sig-1"), "last unproductive signal");
    }

    #[test]
    fn agent_session_counts_aggregates_by_recipient() {
        let db = test_db();
        let (spawn_at, exit_at) = within_window();
        db.record_session_outcome(
            "s-1",
            "shingle",
            &["a".to_string()],
            &spawn_at,
            &exit_at,
            "ok",
            Database::OUTCOME_PRODUCTIVE,
        )
        .unwrap();
        db.record_session_outcome(
            "s-2",
            "shingle",
            &["b".to_string()],
            &spawn_at,
            &exit_at,
            "ok",
            Database::OUTCOME_UNPRODUCTIVE,
        )
        .unwrap();
        db.record_session_outcome(
            "s-3",
            "shingle",
            &["c".to_string()],
            &spawn_at,
            &exit_at,
            "error",
            Database::OUTCOME_ERRORED,
        )
        .unwrap();
        let counts = db
            .agent_session_counts_since("2026-01-01T00:00:00+00:00")
            .unwrap();
        let row = counts.iter().find(|r| r.0 == "shingle").unwrap();
        assert_eq!(row.1, 3, "total = 3");
        assert_eq!(row.2, 1, "productive = 1");
        assert_eq!(row.3, 1, "unproductive = 1");
        assert_eq!(row.4, 1, "errored = 1");
    }

    fn insert_prediction_row(db: &Database, id: &str, updated_at: &str) {
        db.conn
            .execute(
                "INSERT INTO uncertainty_prediction \
                 (id, surface, feature_key, input_fingerprint, model, model_version, \
                  claimed_confidence, prediction_payload, state, cohort_key, \
                  created_at, updated_at) \
                 VALUES (?1, 'legion.task', 'scip.refactor', 'fp-1', 'claude-opus-4-7', \
                         '4.7', 0.7, '{\"predicted_tokens\":1000}', 'emitted', \
                         'legion:claude-opus-4-7:scip.refactor:0.7', ?2, ?3)",
                rusqlite::params![id, updated_at, updated_at],
            )
            .unwrap();
    }

    fn insert_snapshot_row(db: &Database, id: &str, updated_at: &str) {
        db.conn
            .execute(
                "INSERT INTO uncertainty_calibration_snapshot \
                 (id, cohort_key, bucket_lower, bucket_upper, claimed_confidence, \
                  actual_correctness, actual_correctness_raw, prediction_count, \
                  orphan_count, brier_score, computed_at, updated_at) \
                 VALUES (?1, 'legion:claude-opus-4-7:scip.refactor:0.7', 0.6, 0.8, 0.7, \
                         0.68, 0.65, 42, 3, 0.09, ?2, ?2)",
                rusqlite::params![id, updated_at],
            )
            .unwrap();
    }

    #[test]
    fn uncertainty_migration_creates_both_tables() {
        let db = test_db();
        let table_count: i64 = db
            .conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' \
                 AND name IN ('uncertainty_prediction', 'uncertainty_calibration_snapshot')",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(table_count, 2);
    }

    #[test]
    fn uncertainty_migration_creates_orphan_sweep_index() {
        let db = test_db();
        let index_count: i64 = db
            .conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='index' \
                 AND name='idx_uncertainty_prediction_orphan_sweep'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(index_count, 1);
    }

    #[test]
    fn get_uncertainty_prediction_deltas_returns_modified_rows() {
        let db = test_db();
        insert_prediction_row(&db, "p-1", "2026-05-12T00:00:00+00:00");
        insert_prediction_row(&db, "p-2", "2026-05-12T01:00:00+00:00");

        let deltas = db
            .get_uncertainty_prediction_deltas_since("2026-05-12T00:30:00+00:00")
            .unwrap();
        assert_eq!(deltas.len(), 1, "only p-2 is newer than the cutoff");
        assert_eq!(deltas[0].id, "p-2");
        assert_eq!(deltas[0].surface, "legion.task");
        assert_eq!(deltas[0].claimed_confidence, 0.7);
        assert_eq!(deltas[0].state, "emitted");
    }

    #[test]
    fn get_uncertainty_prediction_deltas_includes_soft_deleted() {
        let db = test_db();
        insert_prediction_row(&db, "p-1", "2026-05-12T00:00:00+00:00");
        db.conn
            .execute(
                "UPDATE uncertainty_prediction SET deleted_at = ?1 WHERE id = 'p-1'",
                ["2026-05-12T02:00:00+00:00"],
            )
            .unwrap();
        let deltas = db
            .get_uncertainty_prediction_deltas_since("2026-05-12T01:00:00+00:00")
            .unwrap();
        assert_eq!(deltas.len(), 1);
        assert_eq!(
            deltas[0].deleted_at.as_deref(),
            Some("2026-05-12T02:00:00+00:00")
        );
    }

    #[test]
    fn get_uncertainty_calibration_snapshot_deltas_round_trip() {
        let db = test_db();
        insert_snapshot_row(&db, "s-1", "2026-05-12T00:00:00+00:00");
        insert_snapshot_row(&db, "s-2", "2026-05-12T01:00:00+00:00");

        let deltas = db
            .get_uncertainty_calibration_snapshot_deltas_since("2026-05-12T00:30:00+00:00")
            .unwrap();
        assert_eq!(deltas.len(), 1);
        assert_eq!(deltas[0].id, "s-2");
        assert_eq!(deltas[0].prediction_count, 42);
        assert_eq!(deltas[0].orphan_count, 3);
        assert!((deltas[0].actual_correctness - 0.68).abs() < 1e-9);
        assert!((deltas[0].actual_correctness_raw - 0.65).abs() < 1e-9);
    }

    #[test]
    fn cleanup_tombstones_includes_uncertainty_tables() {
        let db = test_db();
        let old = (Utc::now() - chrono::Duration::days(100)).to_rfc3339();
        insert_prediction_row(&db, "p-old", &old);
        db.conn
            .execute(
                "UPDATE uncertainty_prediction SET deleted_at = ?1 WHERE id = 'p-old'",
                [&old],
            )
            .unwrap();
        insert_snapshot_row(&db, "s-old", &old);
        db.conn
            .execute(
                "UPDATE uncertainty_calibration_snapshot SET deleted_at = ?1 WHERE id = 's-old'",
                [&old],
            )
            .unwrap();

        let result = db.cleanup_tombstones(30).unwrap();
        assert_eq!(result.uncertainty_predictions, 1);
        assert_eq!(result.uncertainty_calibration_snapshots, 1);
    }

    // -- wake_attempts (#487) ------------------------------------------------

    fn wake_id(tag: &str) -> String {
        format!("attempt-{}-{}", tag, uuid::Uuid::now_v7())
    }

    #[test]
    fn wake_attempts_migration_creates_table() {
        let db = test_db();
        // sqlite_master lookup is a stable way to assert the table
        // and indices applied without rebinding to private types.
        let exists: i64 = db
            .conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='wake_attempts'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(exists, 1);
        let indices: i64 = db
            .conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='index' \
                 AND name IN ('idx_wake_attempts_host_state', 'idx_wake_attempts_persona_recent')",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(indices, 2);
    }

    #[test]
    fn enqueue_wake_attempt_inserts_queued_row() {
        use crate::wake_attempts::WakeAttemptState;
        let db = test_db();
        let id = wake_id("enqueue");
        db.enqueue_wake_attempt(&id, "legion", "legion", &["sig-1".to_string()])
            .unwrap();
        let row = db.get_wake_attempt(&id).unwrap().expect("row exists");
        assert_eq!(row.state, WakeAttemptState::Queued);
        assert_eq!(row.persona_id, "legion");
        assert_eq!(row.repo_name, "legion");
        assert_eq!(row.signal_ids, vec!["sig-1".to_string()]);
        assert!(row.acquired_by_host.is_none());
    }

    #[test]
    fn try_claim_wake_attempt_is_atomic_one_winner() {
        let db = test_db();
        let id = wake_id("claim");
        db.enqueue_wake_attempt(&id, "legion", "legion", &[])
            .unwrap();

        let host_a_won = db.try_claim_wake_attempt(&id, "host-a").unwrap();
        let host_b_won = db.try_claim_wake_attempt(&id, "host-b").unwrap();
        // First claimer wins; second sees state != queued and returns false.
        assert!(host_a_won, "first host must win the claim");
        assert!(!host_b_won, "second claim must return false");

        let row = db.get_wake_attempt(&id).unwrap().expect("row");
        assert_eq!(row.acquired_by_host.as_deref(), Some("host-a"));
        assert_eq!(row.state, crate::wake_attempts::WakeAttemptState::Claimed);
    }

    #[test]
    fn try_claim_wake_attempt_rejects_already_claimed() {
        let db = test_db();
        let id = wake_id("re-claim");
        db.enqueue_wake_attempt(&id, "legion", "legion", &[])
            .unwrap();
        assert!(db.try_claim_wake_attempt(&id, "host-a").unwrap());
        // Same host trying twice still loses -- claim is one-shot.
        assert!(!db.try_claim_wake_attempt(&id, "host-a").unwrap());
    }

    #[test]
    fn try_claim_wake_attempt_returns_false_for_unknown_row() {
        let db = test_db();
        assert!(!db.try_claim_wake_attempt("no-such-id", "host-a").unwrap());
    }

    #[test]
    fn transition_wake_attempt_walks_happy_path() {
        use crate::wake_attempts::WakeAttemptState::*;
        let db = test_db();
        let id = wake_id("happy");
        db.enqueue_wake_attempt(&id, "legion", "legion", &[])
            .unwrap();
        assert!(db.try_claim_wake_attempt(&id, "host-a").unwrap());

        db.transition_wake_attempt(&id, Claimed, Spawning).unwrap();
        db.transition_wake_attempt(&id, Spawning, Running).unwrap();
        db.transition_wake_attempt(&id, Running, Exiting).unwrap();
        db.transition_wake_attempt(&id, Exiting, Done).unwrap();

        let row = db.get_wake_attempt(&id).unwrap().expect("row");
        assert_eq!(row.state, Done);
    }

    #[test]
    fn transition_wake_attempt_rejects_illegal_pair() {
        use crate::wake_attempts::WakeAttemptState::*;
        let db = test_db();
        let id = wake_id("illegal");
        db.enqueue_wake_attempt(&id, "legion", "legion", &[])
            .unwrap();
        assert!(db.try_claim_wake_attempt(&id, "host-a").unwrap());

        // Claimed -> Done is not in the table.
        let err = db
            .transition_wake_attempt(&id, Claimed, Done)
            .expect_err("illegal transition must error");
        match err {
            LegionError::IllegalWakeAttemptTransition {
                attempt_id,
                from,
                to,
                ..
            } => {
                assert_eq!(attempt_id, id);
                assert_eq!(from, "claimed");
                assert_eq!(to, "done");
            }
            other => panic!("expected IllegalWakeAttemptTransition, got {other:?}"),
        }
    }

    #[test]
    fn transition_wake_attempt_rejects_stale_from() {
        use crate::wake_attempts::WakeAttemptState::*;
        let db = test_db();
        let id = wake_id("stale");
        db.enqueue_wake_attempt(&id, "legion", "legion", &[])
            .unwrap();
        assert!(db.try_claim_wake_attempt(&id, "host-a").unwrap());
        db.transition_wake_attempt(&id, Claimed, Spawning).unwrap();
        // Caller still thinks the row is `Claimed` but the DB has moved on.
        let err = db
            .transition_wake_attempt(&id, Claimed, Spawning)
            .expect_err("stale `from` must error");
        match err {
            LegionError::IllegalWakeAttemptTransition { current, .. } => {
                assert_eq!(current, "spawning");
            }
            other => panic!("expected IllegalWakeAttemptTransition, got {other:?}"),
        }
    }

    #[test]
    fn transition_wake_attempt_errors_on_missing_row() {
        use crate::wake_attempts::WakeAttemptState::*;
        let db = test_db();
        let err = db
            .transition_wake_attempt("no-such", Queued, Claimed)
            .expect_err("missing row");
        assert!(matches!(err, LegionError::WakeAttemptNotFound(_)));
    }

    #[test]
    fn record_wake_attempt_outcome_sets_terminal_and_status() {
        use crate::wake_attempts::WakeAttemptState::*;
        let db = test_db();
        let id = wake_id("outcome-ok");
        db.enqueue_wake_attempt(&id, "legion", "legion", &[])
            .unwrap();
        db.try_claim_wake_attempt(&id, "host-a").unwrap();
        db.transition_wake_attempt(&id, Claimed, Spawning).unwrap();
        db.transition_wake_attempt(&id, Spawning, Running).unwrap();

        db.record_wake_attempt_outcome(&id, "ok", "productive")
            .unwrap();
        let row = db.get_wake_attempt(&id).unwrap().expect("row");
        assert_eq!(row.state, Done);
        assert_eq!(row.exit_status.as_deref(), Some("ok"));
        assert_eq!(row.outcome.as_deref(), Some("productive"));
        assert!(row.exited_at.is_some());
    }

    #[test]
    fn record_wake_attempt_outcome_maps_error_to_failed() {
        use crate::wake_attempts::WakeAttemptState::*;
        let db = test_db();
        let id = wake_id("outcome-fail");
        db.enqueue_wake_attempt(&id, "legion", "legion", &[])
            .unwrap();
        db.try_claim_wake_attempt(&id, "host-a").unwrap();
        db.record_wake_attempt_outcome(&id, "killed", "errored")
            .unwrap();
        let row = db.get_wake_attempt(&id).unwrap().expect("row");
        assert_eq!(row.state, Failed);
    }

    #[test]
    fn record_wake_attempt_outcome_leaves_terminal_rows_alone() {
        // Late stop hook + already-settled row must not rewrite the
        // outcome. Terminal-is-sticky is the FSM invariant the
        // transition table protects; record_outcome must respect it
        // and surface the rejection as IllegalWakeAttemptTransition
        // with the actual current state, not WakeAttemptNotFound
        // (which would invite a retry loop on a real corruption).
        let db = test_db();
        let id = wake_id("sticky");
        db.enqueue_wake_attempt(&id, "legion", "legion", &[])
            .unwrap();
        db.try_claim_wake_attempt(&id, "host-a").unwrap();
        db.record_wake_attempt_outcome(&id, "ok", "productive")
            .unwrap();
        let err = db
            .record_wake_attempt_outcome(&id, "killed", "errored")
            .expect_err("terminal row must reject re-stamp");
        match err {
            LegionError::IllegalWakeAttemptTransition { current, .. } => {
                assert_eq!(current, "done");
            }
            other => {
                panic!("expected IllegalWakeAttemptTransition with current=done, got {other:?}")
            }
        }
        let row = db.get_wake_attempt(&id).unwrap().expect("row");
        assert_eq!(row.exit_status.as_deref(), Some("ok"));
        assert_eq!(row.outcome.as_deref(), Some("productive"));
    }

    #[test]
    fn list_local_orphans_is_strictly_host_scoped() {
        use crate::wake_attempts::WakeAttemptState::*;
        let db = test_db();

        // This host: one in-flight + one terminal (terminal must NOT appear).
        let local_inflight = wake_id("local-inflight");
        db.enqueue_wake_attempt(&local_inflight, "legion", "legion", &[])
            .unwrap();
        db.try_claim_wake_attempt(&local_inflight, "this-host")
            .unwrap();
        db.transition_wake_attempt(&local_inflight, Claimed, Spawning)
            .unwrap();

        let local_done = wake_id("local-done");
        db.enqueue_wake_attempt(&local_done, "legion", "legion", &[])
            .unwrap();
        db.try_claim_wake_attempt(&local_done, "this-host").unwrap();
        db.record_wake_attempt_outcome(&local_done, "ok", "productive")
            .unwrap();

        // Peer host: in-flight on a DIFFERENT host must NOT appear here.
        let peer_inflight = wake_id("peer-inflight");
        db.enqueue_wake_attempt(&peer_inflight, "legion", "legion", &[])
            .unwrap();
        db.try_claim_wake_attempt(&peer_inflight, "other-host")
            .unwrap();

        let orphans = db.list_local_orphans("this-host").unwrap();
        let ids: Vec<&str> = orphans.iter().map(|r| r.attempt_id.as_str()).collect();
        assert_eq!(
            ids,
            vec![local_inflight.as_str()],
            "only this-host's in-flight row should be returned"
        );
    }

    #[test]
    fn set_wake_attempt_pid_records_pid_and_spawned_at() {
        let db = test_db();
        let id = wake_id("pid");
        db.enqueue_wake_attempt(&id, "legion", "legion", &[])
            .unwrap();
        db.set_wake_attempt_pid(&id, 12345).unwrap();
        let row = db.get_wake_attempt(&id).unwrap().expect("row");
        assert_eq!(row.spawned_pid, Some(12345));
        assert!(row.spawned_at.is_some());
    }

    #[test]
    fn mark_wake_attempt_exit_observed_writes_timestamp() {
        let db = test_db();
        let id = wake_id("exit-obs");
        db.enqueue_wake_attempt(&id, "legion", "legion", &[])
            .unwrap();
        db.mark_wake_attempt_exit_observed(&id).unwrap();
        let row = db.get_wake_attempt(&id).unwrap().expect("row");
        assert!(row.exit_observed_at.is_some());
    }

    #[test]
    fn get_wake_attempt_returns_none_for_missing() {
        let db = test_db();
        assert!(db.get_wake_attempt("no-such").unwrap().is_none());
    }

    #[test]
    fn record_wake_attempt_outcome_rejects_unknown_exit_status() {
        let db = test_db();
        let id = wake_id("bad-status");
        db.enqueue_wake_attempt(&id, "legion", "legion", &[])
            .unwrap();
        db.try_claim_wake_attempt(&id, "host-a").unwrap();
        let err = db
            .record_wake_attempt_outcome(&id, "purple", "productive")
            .expect_err("unknown exit_status must error");
        assert!(matches!(
            err,
            LegionError::IllegalWakeAttemptTransition { .. }
        ));
    }

    #[test]
    fn set_wake_attempt_pid_errors_on_missing_row() {
        let db = test_db();
        let err = db
            .set_wake_attempt_pid("no-such", 1234)
            .expect_err("missing row must error");
        assert!(matches!(err, LegionError::WakeAttemptNotFound(_)));
    }

    #[test]
    fn mark_wake_attempt_exit_observed_errors_on_missing_row() {
        let db = test_db();
        let err = db
            .mark_wake_attempt_exit_observed("no-such")
            .expect_err("missing row must error");
        assert!(matches!(err, LegionError::WakeAttemptNotFound(_)));
    }

    // -- WakeAttemptDelta + apply_wake_attempt_delta (#488) ------------------

    fn delta_for(attempt_id: &str, state: &str, updated_at: &str) -> crate::sync::WakeAttemptDelta {
        crate::sync::WakeAttemptDelta {
            attempt_id: attempt_id.to_string(),
            persona_id: "legion".to_string(),
            repo_name: "legion".to_string(),
            signal_ids: vec!["sig-1".to_string()],
            state: state.to_string(),
            acquired_by_host: Some("peer-host".to_string()),
            acquired_at: Some("2026-05-23T10:00:00Z".to_string()),
            spawned_pid: None,
            spawned_at: None,
            exit_observed_at: None,
            exited_at: None,
            exit_status: None,
            outcome: None,
            deleted_at: None,
            updated_at: updated_at.to_string(),
        }
    }

    #[test]
    fn delta_from_attempt_roundtrips() {
        use crate::wake_attempts::WakeAttemptState;
        let db = test_db();
        let id = wake_id("delta-rt");
        db.enqueue_wake_attempt(&id, "legion", "legion", &["a".into(), "b".into()])
            .unwrap();
        let attempt = db.get_wake_attempt(&id).unwrap().expect("row");
        let delta = crate::sync::WakeAttemptDelta::from_attempt(&attempt);

        assert_eq!(delta.attempt_id, id);
        assert_eq!(delta.state, "queued");
        assert_eq!(delta.signal_ids, vec!["a".to_string(), "b".to_string()]);
        // Round-trip back through serde to confirm the state literal
        // parses on the other side.
        let json = serde_json::to_string(&delta).unwrap();
        let back: crate::sync::WakeAttemptDelta = serde_json::from_str(&json).unwrap();
        assert_eq!(back.state, "queued");
        assert!(matches!(
            WakeAttemptState::parse_state(&back.state).unwrap(),
            WakeAttemptState::Queued
        ));
    }

    #[test]
    fn apply_delta_inserts_when_no_local_row() {
        let db = test_db();
        let delta = delta_for("new-id", "queued", "2026-05-23T10:00:00Z");
        let applied = db.apply_wake_attempt_delta(&delta).unwrap();
        assert!(applied);
        let row = db.get_wake_attempt("new-id").unwrap().expect("row");
        assert_eq!(row.state.as_str(), "queued");
    }

    #[test]
    fn apply_delta_lww_older_is_noop() {
        let db = test_db();
        // Local row is newer.
        let new_delta = delta_for("lww-id", "claimed", "2026-05-23T12:00:00Z");
        assert!(db.apply_wake_attempt_delta(&new_delta).unwrap());
        // Older incoming -> rejected.
        let old_delta = delta_for("lww-id", "queued", "2026-05-23T10:00:00Z");
        assert!(!db.apply_wake_attempt_delta(&old_delta).unwrap());
        let row = db.get_wake_attempt("lww-id").unwrap().expect("row");
        assert_eq!(
            row.state.as_str(),
            "claimed",
            "older delta must not regress"
        );
    }

    #[test]
    fn apply_delta_lww_newer_overwrites() {
        let db = test_db();
        let old = delta_for("over-id", "queued", "2026-05-23T10:00:00Z");
        assert!(db.apply_wake_attempt_delta(&old).unwrap());
        let new = delta_for("over-id", "claimed", "2026-05-23T11:00:00Z");
        assert!(db.apply_wake_attempt_delta(&new).unwrap());
        let row = db.get_wake_attempt("over-id").unwrap().expect("row");
        assert_eq!(row.state.as_str(), "claimed");
    }

    #[test]
    fn apply_delta_terminal_is_sticky_against_non_terminal() {
        // Local row has reached Done; peer's non-terminal delta with a
        // later updated_at must NOT regress us. This is the load-bearing
        // happens-before guard that distinguishes wake_attempts from
        // plain LWW rows.
        let db = test_db();
        let mut done = delta_for("sticky-id", "done", "2026-05-23T11:00:00Z");
        done.exited_at = Some("2026-05-23T11:00:00Z".to_string());
        done.exit_status = Some("ok".to_string());
        done.outcome = Some("productive".to_string());
        assert!(db.apply_wake_attempt_delta(&done).unwrap());

        // Newer updated_at, but state regression: rejected.
        let regress = delta_for("sticky-id", "running", "2026-05-23T12:00:00Z");
        assert!(!db.apply_wake_attempt_delta(&regress).unwrap());

        let row = db.get_wake_attempt("sticky-id").unwrap().expect("row");
        assert_eq!(
            row.state.as_str(),
            "done",
            "terminal must survive a newer non-terminal delta"
        );
    }

    #[test]
    fn apply_delta_both_terminal_disagree_keeps_later_exited_at() {
        let db = test_db();
        let mut early = delta_for("term-id", "done", "2026-05-23T11:00:00Z");
        early.exited_at = Some("2026-05-23T11:00:00Z".to_string());
        early.exit_status = Some("ok".to_string());
        assert!(db.apply_wake_attempt_delta(&early).unwrap());

        // Peer's terminal disagrees but exited later -> wins.
        let mut later_failed = delta_for("term-id", "failed", "2026-05-23T11:30:00Z");
        later_failed.exited_at = Some("2026-05-23T12:00:00Z".to_string());
        later_failed.exit_status = Some("error".to_string());
        assert!(db.apply_wake_attempt_delta(&later_failed).unwrap());

        let row = db.get_wake_attempt("term-id").unwrap().expect("row");
        assert_eq!(row.state.as_str(), "failed");
    }

    #[test]
    fn apply_delta_both_terminal_tie_breaks_on_host() {
        // Local "done" on host-b vs incoming "failed" on host-a. Equal
        // exited_at; deterministic tiebreak picks lower lexicographic
        // host (host-a < host-b).
        let db = test_db();
        let mut local = delta_for("tie-id", "done", "2026-05-23T11:00:00Z");
        local.exited_at = Some("2026-05-23T12:00:00Z".to_string());
        local.exit_status = Some("ok".to_string());
        local.acquired_by_host = Some("host-b".to_string());
        assert!(db.apply_wake_attempt_delta(&local).unwrap());

        let mut peer = delta_for("tie-id", "failed", "2026-05-23T11:00:00Z");
        peer.exited_at = Some("2026-05-23T12:00:00Z".to_string());
        peer.exit_status = Some("error".to_string());
        peer.acquired_by_host = Some("host-a".to_string());
        assert!(db.apply_wake_attempt_delta(&peer).unwrap());

        let row = db.get_wake_attempt("tie-id").unwrap().expect("row");
        assert_eq!(
            row.state.as_str(),
            "failed",
            "host-a wins the lexicographic tiebreak"
        );
    }

    #[test]
    fn apply_delta_unknown_state_is_rejected_no_panic() {
        let db = test_db();
        let delta = delta_for("unknown-id", "frobnicated", "2026-05-23T10:00:00Z");
        let applied = db.apply_wake_attempt_delta(&delta).unwrap();
        assert!(!applied, "forward-incompat state must be rejected");
        assert!(
            db.get_wake_attempt("unknown-id").unwrap().is_none(),
            "rejected delta must not insert"
        );
    }

    #[test]
    fn apply_delta_tombstone_lww() {
        let db = test_db();
        // Local live row.
        let live = delta_for("tomb-id", "running", "2026-05-23T10:00:00Z");
        assert!(db.apply_wake_attempt_delta(&live).unwrap());
        // Incoming tombstone with newer updated_at wins.
        let mut tomb = delta_for("tomb-id", "running", "2026-05-23T11:00:00Z");
        tomb.deleted_at = Some("2026-05-23T11:00:00Z".to_string());
        assert!(db.apply_wake_attempt_delta(&tomb).unwrap());
        let row = db.get_wake_attempt("tomb-id").unwrap().expect("row");
        assert!(row.deleted_at.is_some());
    }

    // -- watch heartbeat tests (#581) -----------------------------------------

    #[test]
    fn upsert_and_get_watch_heartbeat_round_trips() {
        let db = test_db();

        // No heartbeat yet -- get returns None.
        assert!(
            db.get_watch_heartbeat(Some("host-a")).unwrap().is_none(),
            "absent host -> None"
        );
        assert!(
            db.get_watch_heartbeat(None).unwrap().is_none(),
            "empty table -> None"
        );

        // Write a heartbeat.
        db.upsert_watch_heartbeat("host-a", 1234, "0.16.4", 3, Some("spawned 1"))
            .unwrap();

        let beat = db
            .get_watch_heartbeat(Some("host-a"))
            .unwrap()
            .expect("row should exist after upsert");
        assert_eq!(beat.host, "host-a");
        assert_eq!(beat.pid, 1234);
        assert_eq!(beat.version, "0.16.4");
        assert_eq!(beat.repo_count, 3);
        assert_eq!(beat.last_spawn_summary.as_deref(), Some("spawned 1"));

        // Timestamp is an ISO-8601 string.
        assert!(beat.updated_at.contains('T'));

        // get with None should return the same row.
        let any = db
            .get_watch_heartbeat(None)
            .unwrap()
            .expect("any-host query");
        assert_eq!(any.host, "host-a");
    }

    #[test]
    fn upsert_watch_heartbeat_updates_existing_row() {
        let db = test_db();

        db.upsert_watch_heartbeat("host-b", 100, "0.1.0", 2, None)
            .unwrap();
        // Small sleep to ensure updated_at changes.
        std::thread::sleep(std::time::Duration::from_millis(5));
        db.upsert_watch_heartbeat("host-b", 200, "0.2.0", 5, Some("summary"))
            .unwrap();

        let beat = db
            .get_watch_heartbeat(Some("host-b"))
            .unwrap()
            .expect("row");
        assert_eq!(beat.pid, 200, "pid should be updated");
        assert_eq!(beat.version, "0.2.0", "version should be updated");
        assert_eq!(beat.repo_count, 5, "repo_count should be updated");
        assert_eq!(beat.last_spawn_summary.as_deref(), Some("summary"));

        // Only one row for this host (upsert is not insert).
        let count: i64 = db
            .conn
            .query_row(
                "SELECT COUNT(*) FROM watch_heartbeat WHERE host = 'host-b'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 1, "upsert must not insert a duplicate row");
    }

    #[test]
    fn get_watch_heartbeat_none_returns_latest_when_multiple_hosts() {
        let db = test_db();

        db.upsert_watch_heartbeat("host-x", 1, "0.1.0", 1, None)
            .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(5));
        db.upsert_watch_heartbeat("host-y", 2, "0.2.0", 2, None)
            .unwrap();

        // host-y was written last, so get(None) should return host-y.
        let beat = db
            .get_watch_heartbeat(None)
            .unwrap()
            .expect("row from multi-host table");
        assert_eq!(
            beat.host, "host-y",
            "None should return the most recently updated host"
        );
    }

    #[test]
    fn recent_wake_attempts_returns_latest_first() {
        let db = test_db();

        // Seed three wake attempts with different updated_at values.
        let now = chrono::Utc::now();
        let ts1 = (now - chrono::Duration::minutes(10)).to_rfc3339();
        let ts2 = (now - chrono::Duration::minutes(5)).to_rfc3339();
        let ts3 = now.to_rfc3339();

        for (id, ts) in [("att-a", &ts1), ("att-b", &ts2), ("att-c", &ts3)] {
            db.conn
                .execute(
                    "INSERT INTO wake_attempts \
                     (attempt_id, persona_id, repo_name, signal_ids, state, updated_at) \
                     VALUES (?1, 'p', 'r', '[]', 'queued', ?2)",
                    rusqlite::params![id, ts],
                )
                .unwrap();
        }

        let recent = db.recent_wake_attempts(2).unwrap();
        assert_eq!(recent.len(), 2, "limit=2 returns 2 rows");
        assert_eq!(recent[0].attempt_id, "att-c", "newest first");
        assert_eq!(recent[1].attempt_id, "att-b");
    }
}
