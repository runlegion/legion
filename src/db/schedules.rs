//! Schedules: cron-like scheduled commands with active-window support.
//! Owns the `schedules` DDL, the cron/window helpers, and schedule CRUD.

use chrono::{Timelike, Utc};
use rusqlite::Connection;
use uuid::Uuid;

use super::Database;
use crate::error::{LegionError, Result};

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

/// `schedules` table (#-- migration 4).
pub(super) fn create_tables(conn: &Connection) -> Result<()> {
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
    Ok(())
}

/// Column migrations for `schedules`, in their original patch order.
pub(super) fn migrate(conn: &Connection) -> Result<()> {
    // Migration 5: Add time window columns to schedules.
    if !Database::has_column(conn, "schedules", "active_start")? {
        conn.execute_batch("ALTER TABLE schedules ADD COLUMN active_start TEXT;")?;
    }
    if !Database::has_column(conn, "schedules", "active_end")? {
        conn.execute_batch("ALTER TABLE schedules ADD COLUMN active_end TEXT;")?;
    }

    // Migration 13: Soft delete support for multi-node sync (#245).
    if !Database::has_column(conn, "schedules", "deleted_at")? {
        conn.execute_batch("ALTER TABLE schedules ADD COLUMN deleted_at TEXT;")?;
    }

    // Migration 14: Add updated_at for LWW conflict resolution (#255).
    if !Database::has_column(conn, "schedules", "updated_at")? {
        conn.execute_batch(
            "ALTER TABLE schedules ADD COLUMN updated_at TEXT;
                 UPDATE schedules SET updated_at = created_at WHERE updated_at IS NULL;",
        )?;
    }

    // Migration 15: Partial indexes for soft-deleted rows (#256).
    conn.execute_batch(
        "CREATE INDEX IF NOT EXISTS idx_schedules_repo_live \
                 ON schedules(repo) WHERE deleted_at IS NULL;",
    )?;
    Ok(())
}

impl Database {
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
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::db::testutil::test_db;

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
}
