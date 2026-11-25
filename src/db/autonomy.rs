//! Autonomy budget: per-repo weekly action ceilings (#524). Owns the
//! `autonomy_budget` DDL.

use chrono::Utc;
use rusqlite::Connection;

use super::Database;
use crate::error::{LegionError, Result};

/// `autonomy_budget` table (#524).
pub(super) fn create_tables(conn: &Connection) -> Result<()> {
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
    Ok(())
}

impl Database {
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

#[cfg(test)]
mod tests {
    use super::*;

    use crate::db::testutil::test_db;

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
}
