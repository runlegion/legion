//! Watch heartbeat: daemon liveness rows, one per host (#581). Owns the
//! `watch_heartbeat` DDL.

use chrono::Utc;
use rusqlite::{Connection, OptionalExtension};

use super::Database;
use crate::error::{LegionError, Result};

/// `watch_heartbeat` table (#581).
pub(super) fn create_tables(conn: &Connection) -> Result<()> {
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

    /// Whether the watch daemon has beaten within the last `stale_after_secs`
    /// seconds, across any host (#778). A cheap boolean sibling of the
    /// richer `alive | stale | absent` classification `legion watch status`
    /// prints (`cli::watch::classify_beat`) -- callers that only need a
    /// yes/no gate (the delegated-liveness predicate in `wake.rs`) don't
    /// need the age breakdown. An unparseable `updated_at` reads as NOT
    /// alive (fail closed), matching `classify_beat`'s `i64::MAX` coercion.
    ///
    /// Known limitation (accepted, not a bug): this checks the most-recent
    /// beat across ANY host, not the specific host that owns a delegated
    /// attempt (`wake_attempts.acquired_by_host`). In a multi-host cluster
    /// where host-A holds the delegated attempt and dies while host-B keeps
    /// beating, this still reads "alive" even though nothing is left to
    /// finalize host-A's now-orphaned attempt (`reap_dead_pid_attempts` only
    /// scans pids the CALLING host's own tracker knows about). The
    /// single-daemon-down case #778 targets -- one host, one daemon --
    /// is handled correctly; cross-host delegation would need this scoped to
    /// `acquired_by_host` to close the gap.
    pub fn watch_heartbeat_alive(&self, stale_after_secs: u64) -> Result<bool> {
        let beat = match self.get_watch_heartbeat(None)? {
            Some(b) => b,
            None => return Ok(false),
        };
        let age_secs: i64 = chrono::DateTime::parse_from_rfc3339(&beat.updated_at)
            .ok()
            .map(|ts| {
                chrono::Utc::now()
                    .signed_duration_since(ts.with_timezone(&chrono::Utc))
                    .num_seconds()
            })
            .unwrap_or(i64::MAX);
        Ok(age_secs >= 0 && (age_secs as u64) < stale_after_secs)
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
}

#[cfg(test)]
mod tests {
    use crate::db::testutil::test_db;

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

    // -- watch_heartbeat_alive (#778) -----------------------------------------

    #[test]
    fn watch_heartbeat_alive_false_when_no_row() {
        let db = test_db();
        assert!(
            !db.watch_heartbeat_alive(120).unwrap(),
            "no heartbeat ever written -> not alive"
        );
    }

    #[test]
    fn watch_heartbeat_alive_true_for_fresh_beat() {
        let db = test_db();
        db.upsert_watch_heartbeat("host-a", 1, "0.1.0", 1, None)
            .unwrap();
        assert!(
            db.watch_heartbeat_alive(120).unwrap(),
            "a beat written just now is within any sane stale window"
        );
    }

    #[test]
    fn watch_heartbeat_alive_false_for_stale_beat() {
        let db = test_db();
        db.upsert_watch_heartbeat("host-a", 1, "0.1.0", 1, None)
            .unwrap();
        // Backdate the beat past the stale window.
        let old = (chrono::Utc::now() - chrono::Duration::seconds(300)).to_rfc3339();
        db.conn
            .execute(
                "UPDATE watch_heartbeat SET updated_at = ?1 WHERE host = 'host-a'",
                rusqlite::params![old],
            )
            .unwrap();
        assert!(
            !db.watch_heartbeat_alive(120).unwrap(),
            "a beat 300s old must read as stale under a 120s window"
        );
    }

    #[test]
    fn watch_heartbeat_alive_false_for_unparseable_timestamp() {
        let db = test_db();
        db.upsert_watch_heartbeat("host-a", 1, "0.1.0", 1, None)
            .unwrap();
        db.conn
            .execute(
                "UPDATE watch_heartbeat SET updated_at = 'not-a-timestamp' WHERE host = 'host-a'",
                [],
            )
            .unwrap();
        assert!(
            !db.watch_heartbeat_alive(120).unwrap(),
            "a corrupt timestamp must fail closed (not alive), never panic"
        );
    }
}
