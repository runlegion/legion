//! Aggregate statistics and dashboard queries over reflections.

use super::reflections::{REFLECTION_COLUMNS, map_reflection_row};
use super::{Database, Reflection};
use crate::error::{LegionError, Result};

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

impl Database {
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

    /// Get high-value reflections from other repos (by recall_count).
    ///
    /// Returns reflections with recall_count > 0 from repos other than
    /// the given one, ordered by recall_count descending. `range` applies
    /// #786's `created_at` predicate directly in the WHERE clause
    /// (`TimeRange::default()` is unbounded, a no-op).
    pub fn get_high_value_cross_repo(
        &self,
        exclude_repo: &str,
        limit: usize,
        range: &crate::timerange::TimeRange,
    ) -> Result<Vec<Reflection>> {
        let range_clause = crate::timerange::TimeRange::sql_clause(3);
        let sql = format!(
            "SELECT {REFLECTION_COLUMNS} \
             FROM reflections WHERE repo != ?1 AND recall_count > 0 AND deleted_at IS NULL{range_clause} \
             ORDER BY recall_count DESC LIMIT ?2"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(
            rusqlite::params![
                exclude_repo,
                limit,
                range.since_bound()?,
                range.until_bound()?
            ],
            map_reflection_row,
        )?;
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
}

#[cfg(test)]
mod tests {
    use crate::db::testutil::test_db;

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
}
