//! Statusline samples: rate-limit and usage telemetry captured on every
//! Claude Code statusline render (#287). Owns the `rate_limit_samples`
//! and `usage_samples` DDL.

use rusqlite::Connection;

use super::Database;
use crate::error::{LegionError, Result};

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

/// `rate_limit_samples` and `usage_samples` tables (#287).
pub(super) fn create_tables(conn: &Connection) -> Result<()> {
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
    Ok(())
}

impl Database {
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
}

#[cfg(test)]
mod tests {
    use crate::db::testutil::test_db;

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
}
