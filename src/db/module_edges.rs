//! Module graph edges: one row per resolved (or unresolved) import (#710).
//!
//! `module_edges` stores the output of `graph::build_module_graph`: for every
//! static import/export-from/re-export and every literal dynamic `import()`
//! found in a js/ts/jsx/tsx file, one row records the specifier text and
//! where (if anywhere) it resolved to. This is the substrate `sym` uses to
//! answer "which files import X" / "what does X import" -- questions SCIP
//! symbol references cannot answer, because they are about the module graph,
//! not a symbol.

use rusqlite::Connection;

use super::Database;
use crate::error::Result;
use crate::graph::ImportEdge;

/// Create the `module_edges` table and its lookup indexes.
///
/// Called from `init_schema` inside the schema-creation transaction.
/// Idempotent via `IF NOT EXISTS`.
///
/// No single-column primary key fits: a file can import the same specifier
/// only once in practice (`requested_modules` is keyed by specifier text
/// already), but a repo can re-index and re-resolve the same edge, so the
/// natural key is `(repo, from_path, specifier)` -- re-running the graph
/// build for a file updates its edges in place rather than duplicating them.
pub(super) fn create_tables(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS module_edges (
            repo      TEXT NOT NULL,
            from_path TEXT NOT NULL,
            specifier TEXT NOT NULL,
            to_path   TEXT,
            PRIMARY KEY (repo, from_path, specifier)
        );
        CREATE INDEX IF NOT EXISTS idx_module_edges_repo_to
            ON module_edges(repo, to_path);",
    )?;
    Ok(())
}

impl Database {
    /// Upsert a batch of module edges, keyed on `(repo, from_path, specifier)`.
    ///
    /// Idempotent: re-running the graph build for a file updates its edges'
    /// resolution in place rather than duplicating them. All rows are
    /// written inside a single transaction, through one prepared statement,
    /// for throughput.
    pub fn upsert_module_edges(&self, edges: &[ImportEdge]) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        {
            let mut stmt = tx.prepare(
                "INSERT INTO module_edges (repo, from_path, specifier, to_path)
                 VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(repo, from_path, specifier) DO UPDATE SET
                     to_path = excluded.to_path",
            )?;
            for edge in edges {
                stmt.execute(rusqlite::params![
                    &edge.repo,
                    &edge.from,
                    &edge.specifier,
                    &edge.to,
                ])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Return every edge whose importer is `from_path` -- "what does this
    /// file import" -- ordered by specifier for stable output.
    pub fn list_module_edges_from(&self, repo: &str, from_path: &str) -> Result<Vec<ImportEdge>> {
        let mut stmt = self.conn.prepare(
            "SELECT repo, from_path, specifier, to_path
             FROM module_edges
             WHERE repo = ?1 AND from_path = ?2
             ORDER BY specifier",
        )?;
        let rows = stmt
            .query_map(rusqlite::params![repo, from_path], row_to_edge)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Return every edge that resolved to `to_path` -- "what imports this
    /// file" -- ordered by importer path for stable output. Edges with
    /// `to_path IS NULL` (unresolved/external) never match; there is
    /// nothing to look up an importee for.
    pub fn list_module_edges_to(&self, repo: &str, to_path: &str) -> Result<Vec<ImportEdge>> {
        let mut stmt = self.conn.prepare(
            "SELECT repo, from_path, specifier, to_path
             FROM module_edges
             WHERE repo = ?1 AND to_path = ?2
             ORDER BY from_path",
        )?;
        let rows = stmt
            .query_map(rusqlite::params![repo, to_path], row_to_edge)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Delete edges for `repo` whose importer is not in `live_from_paths`.
    ///
    /// Mirrors `prune_file_inventory`: call after `upsert_module_edges` on a
    /// full repo re-walk so edges from deleted/renamed files do not linger.
    /// When `live_from_paths` is empty every edge for the repo is removed.
    pub fn prune_module_edges(&self, repo: &str, live_from_paths: &[&str]) -> Result<usize> {
        if live_from_paths.is_empty() {
            let n = self.conn.execute(
                "DELETE FROM module_edges WHERE repo = ?1",
                rusqlite::params![repo],
            )?;
            return Ok(n);
        }

        self.conn.execute_batch(
            "CREATE TEMP TABLE IF NOT EXISTS _module_edges_live_from (path TEXT PRIMARY KEY)",
        )?;
        self.conn
            .execute_batch("DELETE FROM _module_edges_live_from")?;

        {
            let tx = self.conn.unchecked_transaction()?;
            {
                let mut insert =
                    tx.prepare("INSERT OR IGNORE INTO _module_edges_live_from (path) VALUES (?1)")?;
                for p in live_from_paths {
                    insert.execute(rusqlite::params![p])?;
                }
            }
            tx.commit()?;
        }

        let n = self.conn.execute(
            "DELETE FROM module_edges
             WHERE repo = ?1
               AND from_path NOT IN (SELECT path FROM _module_edges_live_from)",
            rusqlite::params![repo],
        )?;
        Ok(n)
    }
}

fn row_to_edge(row: &rusqlite::Row<'_>) -> rusqlite::Result<ImportEdge> {
    Ok(ImportEdge {
        repo: row.get(0)?,
        from: row.get(1)?,
        specifier: row.get(2)?,
        to: row.get(3)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::testutil::test_db;

    fn edge(repo: &str, from: &str, specifier: &str, to: Option<&str>) -> ImportEdge {
        ImportEdge {
            repo: repo.to_string(),
            from: from.to_string(),
            specifier: specifier.to_string(),
            to: to.map(|s| s.to_string()),
        }
    }

    #[test]
    fn upsert_and_list_from_round_trip() {
        let db = test_db();
        let edges = vec![
            edge("r", "src/a.ts", "./b", Some("src/b.ts")),
            edge("r", "src/a.ts", "lodash", None),
        ];
        db.upsert_module_edges(&edges).unwrap();

        let got = db.list_module_edges_from("r", "src/a.ts").unwrap();
        assert_eq!(got.len(), 2);
        // Ordered by specifier: "./b" before "lodash".
        assert_eq!(got[0].specifier, "./b");
        assert_eq!(got[0].to.as_deref(), Some("src/b.ts"));
        assert_eq!(got[1].specifier, "lodash");
        assert_eq!(
            got[1].to, None,
            "unresolved/external must be None, not dropped"
        );
    }

    #[test]
    fn list_to_answers_importee_query() {
        let db = test_db();
        db.upsert_module_edges(&[
            edge("r", "src/a.ts", "./b", Some("src/b.ts")),
            edge("r", "src/c.ts", "./b", Some("src/b.ts")),
            edge("r", "src/d.ts", "./e", Some("src/e.ts")),
        ])
        .unwrap();

        let got = db.list_module_edges_to("r", "src/b.ts").unwrap();
        assert_eq!(got.len(), 2);
        let importers: Vec<&str> = got.iter().map(|e| e.from.as_str()).collect();
        assert!(importers.contains(&"src/a.ts"));
        assert!(importers.contains(&"src/c.ts"));
    }

    #[test]
    fn upsert_is_idempotent_and_updates_resolution() {
        let db = test_db();
        db.upsert_module_edges(&[edge("r", "src/a.ts", "./b", None)])
            .unwrap();
        // Re-run after the file appears: resolution flips from None to Some.
        db.upsert_module_edges(&[edge("r", "src/a.ts", "./b", Some("src/b.ts"))])
            .unwrap();

        let got = db.list_module_edges_from("r", "src/a.ts").unwrap();
        assert_eq!(got.len(), 1, "re-run must not duplicate the edge");
        assert_eq!(got[0].to.as_deref(), Some("src/b.ts"));
    }

    #[test]
    fn prune_removes_edges_for_deleted_importers() {
        let db = test_db();
        db.upsert_module_edges(&[
            edge("r", "src/a.ts", "./b", Some("src/b.ts")),
            edge("r", "src/gone.ts", "./b", Some("src/b.ts")),
        ])
        .unwrap();

        let pruned = db.prune_module_edges("r", &["src/a.ts"]).unwrap();
        assert_eq!(pruned, 1);

        assert!(
            db.list_module_edges_from("r", "src/gone.ts")
                .unwrap()
                .is_empty()
        );
        assert_eq!(db.list_module_edges_from("r", "src/a.ts").unwrap().len(), 1);
    }

    #[test]
    fn prune_empty_live_paths_wipes_repo() {
        let db = test_db();
        db.upsert_module_edges(&[edge("r", "src/a.ts", "./b", Some("src/b.ts"))])
            .unwrap();

        let pruned = db.prune_module_edges("r", &[]).unwrap();
        assert_eq!(pruned, 1);
        assert!(
            db.list_module_edges_from("r", "src/a.ts")
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn scoped_to_repo() {
        let db = test_db();
        db.upsert_module_edges(&[
            edge("alpha", "src/a.ts", "./b", Some("src/b.ts")),
            edge("beta", "src/a.ts", "./b", Some("src/b.ts")),
        ])
        .unwrap();

        let got = db.list_module_edges_from("alpha", "src/a.ts").unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].repo, "alpha");
    }
}
