//! Health samples: per-host system telemetry (#88). Owns the
//! `health_samples` DDL.

use rusqlite::Connection;

use super::Database;
use crate::error::{LegionError, Result};

/// `health_samples` table (#88).
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
            CREATE INDEX IF NOT EXISTS idx_health_sampled ON health_samples(sampled_at);",
    )?;
    Ok(())
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

impl Database {
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
}
