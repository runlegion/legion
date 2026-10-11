//! Health samples: per-host system telemetry (#88), and `host_status`, the
//! one synced row per host the mesh ranks from. Owns both DDLs.

use rusqlite::Connection;

use super::Database;
use crate::error::{LegionError, Result};

/// `health_samples` table (#88) and the synced `host_status` table.
pub(super) fn create_tables(conn: &Connection) -> Result<()> {
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
            CREATE INDEX IF NOT EXISTS idx_health_sampled ON health_samples(sampled_at);
            CREATE TABLE IF NOT EXISTS host_status (
                hostname      TEXT PRIMARY KEY,
                cpu_pct       REAL NOT NULL,
                mem_pct       REAL NOT NULL,
                load_1m       REAL,
                agents_active INTEGER NOT NULL,
                created_at    TEXT NOT NULL,
                updated_at    TEXT NOT NULL
            );",
    )?;
    Ok(())
}

/// The `health_samples` columns in the order [`map_health_row`] reads them.
/// One list for the insert and every select, so a new column cannot be
/// written by one and missed by another.
const HEALTH_COLS: &str = "id, hostname, sampled_at, cpu_usage_pct, load_avg_1, load_avg_5, \
     load_avg_15, cpu_core_count, mem_total_bytes, mem_used_bytes, mem_usage_pct, \
     swap_total_bytes, swap_used_bytes, cpu_temp_celsius, agents_active, pressure";

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

/// Map a database row to a HostStatus struct.
fn map_host_status_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<crate::health::HostStatus> {
    Ok(crate::health::HostStatus {
        hostname: row.get(0)?,
        cpu_pct: row.get(1)?,
        mem_pct: row.get(2)?,
        load_1m: row.get(3)?,
        agents_active: row.get(4)?,
        created_at: row.get(5)?,
        updated_at: row.get(6)?,
    })
}

impl Database {
    // -- Health Samples ----------------------------------------------------------

    /// Insert a health sample into the database.
    pub fn insert_health_sample(&self, sample: &crate::health::HealthSample) -> Result<String> {
        self.conn.execute(
            &format!(
                "INSERT INTO health_samples ({HEALTH_COLS}) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)"
            ),
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

    /// Get health samples for a hostname since a given timestamp.
    pub fn get_health_history(
        &self,
        hostname: &str,
        since: &str,
    ) -> Result<Vec<crate::health::HealthSample>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {HEALTH_COLS} FROM health_samples WHERE hostname = ?1 AND sampled_at > ?2 \
             ORDER BY sampled_at DESC"
        ))?;
        let rows = stmt.query_map(rusqlite::params![hostname, since], map_health_row)?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(LegionError::Database)
    }

    /// Get health samples from all hostnames since a given timestamp.
    pub fn get_health_all_hosts(&self, since: &str) -> Result<Vec<crate::health::HealthSample>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {HEALTH_COLS} FROM health_samples WHERE sampled_at > ?1 \
             ORDER BY sampled_at DESC"
        ))?;
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

    // -- Host Status ---------------------------------------------------------------

    /// Write this host's current load as its one `host_status` row.
    ///
    /// `created_at` is kept from the first write; `updated_at` moves on every
    /// write, so gossip's newer-wins ordering always prefers the latest sample
    /// and the mesh ranker can age the row.
    pub fn upsert_host_status(&self, status: &crate::health::HostStatus) -> Result<()> {
        self.conn.execute(
            "INSERT INTO host_status \
             (hostname, cpu_pct, mem_pct, load_1m, agents_active, created_at, updated_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7) \
             ON CONFLICT(hostname) DO UPDATE SET cpu_pct = excluded.cpu_pct, \
             mem_pct = excluded.mem_pct, load_1m = excluded.load_1m, \
             agents_active = excluded.agents_active, updated_at = excluded.updated_at",
            rusqlite::params![
                status.hostname,
                status.cpu_pct,
                status.mem_pct,
                status.load_1m,
                status.agents_active,
                status.created_at,
                status.updated_at,
            ],
        )?;
        Ok(())
    }

    /// Every host's `host_status` row, local and gossiped.
    pub fn list_host_status(&self) -> Result<Vec<crate::health::HostStatus>> {
        let mut stmt = self.conn.prepare(
            "SELECT hostname, cpu_pct, mem_pct, load_1m, agents_active, created_at, updated_at \
             FROM host_status ORDER BY hostname",
        )?;
        let rows = stmt.query_map([], map_host_status_row)?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(LegionError::Database)
    }
}

#[cfg(test)]
mod tests {
    use crate::db::testutil::test_db;
    use uuid::Uuid;

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

        let history = db
            .get_health_history("macbook", "2026-04-03T09:00:00Z")
            .unwrap();
        assert_eq!(history.len(), 1);
        let latest = &history[0];
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

    fn host(name: &str, cpu: f64, mem: f64, at: &str) -> crate::health::HostStatus {
        crate::health::HostStatus {
            hostname: name.to_string(),
            cpu_pct: cpu,
            mem_pct: mem,
            load_1m: Some(1.0),
            agents_active: 1,
            created_at: at.to_string(),
            updated_at: at.to_string(),
        }
    }

    #[test]
    fn upsert_host_status_keeps_one_row_per_host() {
        let db = test_db();
        db.upsert_host_status(&host("puck", 10.0, 20.0, "2026-10-10T10:00:00+00:00"))
            .unwrap();
        db.upsert_host_status(&host("puck", 70.0, 30.0, "2026-10-10T10:01:00+00:00"))
            .unwrap();
        db.upsert_host_status(&host("laptop", 5.0, 5.0, "2026-10-10T10:01:00+00:00"))
            .unwrap();

        let rows = db.list_host_status().unwrap();
        assert_eq!(rows.len(), 2);
        let puck = rows.iter().find(|r| r.hostname == "puck").unwrap();
        assert!((puck.cpu_pct - 70.0).abs() < f64::EPSILON);
        assert_eq!(puck.updated_at, "2026-10-10T10:01:00+00:00");
        assert_eq!(
            puck.created_at, "2026-10-10T10:00:00+00:00",
            "created_at is kept from the first write"
        );
    }
}
