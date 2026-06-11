//! Agent session log (#389): classify watch-spawned sessions as
//! productive or not and aggregate per-recipient counts. Owns the
//! `agent_session_log` DDL.

use rusqlite::{Connection, OptionalExtension};

use super::Database;
use crate::error::{LegionError, Result};

/// `agent_session_log` table (#389).
pub(super) fn create_tables(conn: &Connection) -> Result<()> {
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
    Ok(())
}

impl Database {
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
}

#[cfg(test)]
mod tests {
    use super::*;

    use chrono::Utc;

    use crate::db::ReflectionMeta;
    use crate::db::testutil::test_db;

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
}
