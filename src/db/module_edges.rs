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
    /// Replace module edges for a batch of just-(re)parsed files, keyed on
    /// `(repo, from_path, specifier)`.
    ///
    /// `parsed_from_paths` must list every file whose edges were (re)computed
    /// this call -- including one that now produces zero edges (its last
    /// import was just deleted, or it never had any). Every existing row for
    /// each of those files is deleted before `edges` is inserted, in the same
    /// transaction. The delete set has to come from `parsed_from_paths`, not
    /// from `edges` itself: a file that shrinks to zero imports contributes
    /// no rows to `edges` at all, so nothing in the edges list would point
    /// back at it to clear its now-stale rows, and `prune_module_edges` does
    /// not catch this either -- it only removes edges for files that are no
    /// longer live, not files whose import set changed while staying live.
    ///
    /// The `ON CONFLICT` clause is a defensive no-op once the delete above
    /// has run for every path in `parsed_from_paths`; it only matters if a
    /// caller passes an edge whose `from` is outside `parsed_from_paths`,
    /// which should not happen but would otherwise duplicate a row instead
    /// of erroring.
    pub fn upsert_module_edges(
        &self,
        repo: &str,
        parsed_from_paths: &[&str],
        edges: &[ImportEdge],
    ) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        {
            let mut delete_stmt =
                tx.prepare("DELETE FROM module_edges WHERE repo = ?1 AND from_path = ?2")?;
            for from_path in parsed_from_paths {
                delete_stmt.execute(rusqlite::params![repo, from_path])?;
            }

            let mut insert_stmt = tx.prepare(
                "INSERT INTO module_edges (repo, from_path, specifier, to_path)
                 VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(repo, from_path, specifier) DO UPDATE SET
                     to_path = excluded.to_path",
            )?;
            for edge in edges {
                insert_stmt.execute(rusqlite::params![
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
    ///
    /// Not yet wired to a CLI surface -- #710 only populates the table via
    /// `legion index` (see `handle_index` in `src/cli/index_cmd.rs`). No
    /// issue is filed yet for the query commands (`sym imports` /
    /// `sym importers`, or similar) that would call this; this is the query
    /// API they will use once one is. Exercised directly by this module's
    /// tests in the meantime.
    #[allow(dead_code)]
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
    ///
    /// Not yet wired to a CLI surface; see `list_module_edges_from`.
    #[allow(dead_code)]
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
        db.upsert_module_edges("r", &["src/a.ts"], &edges).unwrap();

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
        db.upsert_module_edges(
            "r",
            &["src/a.ts", "src/c.ts", "src/d.ts"],
            &[
                edge("r", "src/a.ts", "./b", Some("src/b.ts")),
                edge("r", "src/c.ts", "./b", Some("src/b.ts")),
                edge("r", "src/d.ts", "./e", Some("src/e.ts")),
            ],
        )
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
        db.upsert_module_edges("r", &["src/a.ts"], &[edge("r", "src/a.ts", "./b", None)])
            .unwrap();
        // Re-run after the file appears: resolution flips from None to Some.
        db.upsert_module_edges(
            "r",
            &["src/a.ts"],
            &[edge("r", "src/a.ts", "./b", Some("src/b.ts"))],
        )
        .unwrap();

        let got = db.list_module_edges_from("r", "src/a.ts").unwrap();
        assert_eq!(got.len(), 1, "re-run must not duplicate the edge");
        assert_eq!(got[0].to.as_deref(), Some("src/b.ts"));
    }

    #[test]
    fn reindex_drops_a_removed_specifier_from_a_still_live_file() {
        let db = test_db();
        db.upsert_module_edges(
            "r",
            &["src/a.ts"],
            &[
                edge("r", "src/a.ts", "./b", Some("src/b.ts")),
                edge("r", "src/a.ts", "./c", Some("src/c.ts")),
            ],
        )
        .unwrap();

        // Re-index after the file drops its `./c` import. The file is still
        // live (still passed in `parsed_from_paths`), so `prune_module_edges`
        // never sees it -- the stale `./c` edge must be dropped by the
        // upsert itself, not left to linger forever.
        db.upsert_module_edges(
            "r",
            &["src/a.ts"],
            &[edge("r", "src/a.ts", "./b", Some("src/b.ts"))],
        )
        .unwrap();

        let got = db.list_module_edges_from("r", "src/a.ts").unwrap();
        assert_eq!(
            got.len(),
            1,
            "the removed ./c specifier must not survive re-indexing, got {got:?}"
        );
        assert_eq!(got[0].specifier, "./b");
    }

    #[test]
    fn reindex_clears_all_edges_when_a_live_file_drops_every_import() {
        let db = test_db();
        db.upsert_module_edges(
            "r",
            &["src/a.ts"],
            &[edge("r", "src/a.ts", "./b", Some("src/b.ts"))],
        )
        .unwrap();

        // The file still exists (still passed as a parsed path) but now has
        // no imports at all -- build_module_graph produces zero edges for
        // it, not an edge with `to: None`. Nothing in `edges` references
        // "src/a.ts" to trigger a delete unless the delete is keyed on
        // `parsed_from_paths` rather than on the edges list.
        db.upsert_module_edges("r", &["src/a.ts"], &[]).unwrap();

        assert!(
            db.list_module_edges_from("r", "src/a.ts")
                .unwrap()
                .is_empty(),
            "a live file with zero current imports must have zero edges after re-index"
        );
    }

    #[test]
    fn prune_removes_edges_for_deleted_importers() {
        let db = test_db();
        db.upsert_module_edges(
            "r",
            &["src/a.ts", "src/gone.ts"],
            &[
                edge("r", "src/a.ts", "./b", Some("src/b.ts")),
                edge("r", "src/gone.ts", "./b", Some("src/b.ts")),
            ],
        )
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
        db.upsert_module_edges(
            "r",
            &["src/a.ts"],
            &[edge("r", "src/a.ts", "./b", Some("src/b.ts"))],
        )
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
        db.upsert_module_edges(
            "alpha",
            &["src/a.ts"],
            &[edge("alpha", "src/a.ts", "./b", Some("src/b.ts"))],
        )
        .unwrap();
        db.upsert_module_edges(
            "beta",
            &["src/a.ts"],
            &[edge("beta", "src/a.ts", "./b", Some("src/b.ts"))],
        )
        .unwrap();

        let got = db.list_module_edges_from("alpha", "src/a.ts").unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].repo, "alpha");
    }
}
