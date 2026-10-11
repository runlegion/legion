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

    /// Most recent non-null model id observed for a session (#831).
    ///
    /// The statusline render is the only place legion sees the live model
    /// id, so this is the authoritative answer to "what is producing this
    /// session's work". Hooks have no model field on their stdin payload,
    /// which is why the uncertainty emit path resolves through here rather
    /// than accepting a caller-supplied default that rots when the harness
    /// changes its default model.
    ///
    /// Rows with a NULL model are skipped rather than terminating the
    /// search: an older sample carrying a real id is a better answer than
    /// the newest sample carrying none.
    pub fn latest_model_for_session(&self, session_id: &str) -> Result<Option<String>> {
        let mut stmt = self.conn.prepare(
            "SELECT model FROM rate_limit_samples \
             WHERE deleted_at IS NULL AND session_id = ?1 AND model IS NOT NULL \
             ORDER BY sampled_at DESC LIMIT 1",
        )?;
        let mut rows = stmt.query(rusqlite::params![session_id])?;
        match rows.next().map_err(LegionError::Database)? {
            Some(row) => Ok(row.get::<_, Option<String>>(0)?),
            None => Ok(None),
        }
    }
}
