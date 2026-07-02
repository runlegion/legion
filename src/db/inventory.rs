//! File inventory: per-file metadata for every watched repo (#705).
//!
//! `file_inventory` stores one row per file encountered during `legion index`.
//! It is the always-fresh substrate for `sym tree` and `sym etc find-file`
//! (#706, #709). The walk that populates it lives in `src/inventory.rs`;
//! this module owns only the DDL and the persistence API.

use rusqlite::Connection;
use serde::{Deserialize, Serialize};

use super::Database;
use crate::error::Result;

/// One row in the `file_inventory` table.
///
/// Paths are always repo-relative (forward-slash separated, no leading slash).
/// `lang` is the SCIP language tag (e.g. "rust", "typescript"); `None` for
/// files that SCIP does not index (docs, configs, scripts, etc.).
/// `symbol_count` is 0 until a later epic populates it from the SCIP blob.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FileInventoryEntry {
    pub repo: String,
    /// Repo-relative path with forward-slash separators, no leading slash.
    pub path: String,
    /// File extension without the leading dot (`rs`, `md`, `toml`). `None`
    /// for dotfiles or files with no extension.
    pub ext: Option<String>,
    /// SCIP language tag for files whose extension maps to an indexed
    /// language; `None` for everything else.
    pub lang: Option<String>,
    pub size: u64,
    /// RFC3339 last-modified timestamp from the file system.
    pub mtime: String,
    /// Number of SCIP symbols in this file; 0 when no SCIP index covers it.
    pub symbol_count: u32,
}

/// Optional scope restrictions for [`Database::list_file_inventory`].
///
/// `None` on any field disables that filter; `InventoryFilter::default()`
/// returns every row.
///
/// Used by `sym tree` (#706) and `sym etc find-file` (#709); `#[allow]`
/// silences the dead-code lint until those callers land.
#[derive(Debug, Default)]
#[allow(dead_code)]
pub struct InventoryFilter<'a> {
    pub repo: Option<&'a str>,
    pub ext: Option<&'a str>,
    pub lang: Option<&'a str>,
}

/// Create the `file_inventory` table and its covering indexes.
///
/// Called from `init_schema` inside the schema-creation transaction.
/// Idempotent via `IF NOT EXISTS`.
pub(super) fn create_tables(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS file_inventory (
            repo TEXT NOT NULL,
            path TEXT NOT NULL,
            ext  TEXT,
            lang TEXT,
            size INTEGER NOT NULL,
            mtime TEXT NOT NULL,
            symbol_count INTEGER NOT NULL DEFAULT 0,
            PRIMARY KEY (repo, path)
        );
        CREATE INDEX IF NOT EXISTS idx_file_inventory_repo
            ON file_inventory(repo);
        CREATE INDEX IF NOT EXISTS idx_file_inventory_repo_ext
            ON file_inventory(repo, ext);",
    )?;
    Ok(())
}

impl Database {
    /// Upsert a batch of file inventory rows, keyed on `(repo, path)`.
    ///
    /// Idempotent: re-indexing the same file updates its metadata in place
    /// without changing the row identity. All rows are written inside a
    /// single transaction for throughput.
    pub fn upsert_file_inventory(&self, entries: &[FileInventoryEntry]) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        for entry in entries {
            tx.execute(
                "INSERT INTO file_inventory (repo, path, ext, lang, size, mtime, symbol_count)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                 ON CONFLICT(repo, path) DO UPDATE SET
                     ext          = excluded.ext,
                     lang         = excluded.lang,
                     size         = excluded.size,
                     mtime        = excluded.mtime,
                     symbol_count = excluded.symbol_count",
                rusqlite::params![
                    &entry.repo,
                    &entry.path,
                    &entry.ext,
                    &entry.lang,
                    entry.size as i64,
                    &entry.mtime,
                    entry.symbol_count as i64,
                ],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Return inventory rows matching the optional filter, ordered by
    /// `(repo, path)`.
    ///
    /// `None` on any filter field disables that restriction. All three
    /// `None` returns every row in the table.
    ///
    /// Read path for `sym tree` (#706) and `sym etc find-file` (#709);
    /// `#[allow]` silences the dead-code lint until those callers land.
    #[allow(dead_code)]
    pub fn list_file_inventory(
        &self,
        filter: &InventoryFilter<'_>,
    ) -> Result<Vec<FileInventoryEntry>> {
        let mut sql = String::from(
            "SELECT repo, path, ext, lang, size, mtime, symbol_count
             FROM file_inventory
             WHERE 1=1",
        );
        let mut params: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
        if let Some(r) = filter.repo {
            sql.push_str(" AND repo = ?");
            params.push(Box::new(r.to_string()));
        }
        if let Some(e) = filter.ext {
            sql.push_str(" AND ext = ?");
            params.push(Box::new(e.to_string()));
        }
        if let Some(l) = filter.lang {
            sql.push_str(" AND lang = ?");
            params.push(Box::new(l.to_string()));
        }
        sql.push_str(" ORDER BY repo, path");

        let mut stmt = self.conn.prepare(&sql)?;
        let param_refs: Vec<&dyn rusqlite::ToSql> = params.iter().map(|p| p.as_ref()).collect();
        let rows = stmt
            .query_map(rusqlite::params_from_iter(param_refs), |row| {
                Ok(FileInventoryEntry {
                    repo: row.get(0)?,
                    path: row.get(1)?,
                    ext: row.get(2)?,
                    lang: row.get(3)?,
                    size: row.get::<_, i64>(4)? as u64,
                    mtime: row.get(5)?,
                    symbol_count: row.get::<_, i64>(6)? as u32,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Delete rows for `repo` whose path is not in `live_paths`.
    ///
    /// Call this after [`upsert_file_inventory`] to evict stale rows left
    /// by deleted or renamed files. Returns the number of rows deleted.
    ///
    /// When `live_paths` is empty every row for the repo is removed (the
    /// repo's working directory became empty or was deleted).
    pub fn prune_file_inventory(&self, repo: &str, live_paths: &[String]) -> Result<usize> {
        if live_paths.is_empty() {
            let n = self.conn.execute(
                "DELETE FROM file_inventory WHERE repo = ?1",
                rusqlite::params![repo],
            )?;
            return Ok(n);
        }

        // Materialize live paths into a temporary table so the DELETE is a
        // single statement regardless of how many files the repo has. A NOT
        // IN (?1, ?2, ...) approach hits SQLite's variable-number limit for
        // repos with many files; a temp table has no such bound.
        self.conn.execute_batch(
            "CREATE TEMP TABLE IF NOT EXISTS _inv_live_paths (path TEXT PRIMARY KEY)",
        )?;
        self.conn.execute_batch("DELETE FROM _inv_live_paths")?;

        {
            let tx = self.conn.unchecked_transaction()?;
            {
                // The Statement borrows `tx`; drop it before commit.
                let mut insert =
                    tx.prepare("INSERT OR IGNORE INTO _inv_live_paths (path) VALUES (?1)")?;
                for p in live_paths {
                    insert.execute(rusqlite::params![p])?;
                }
            }
            tx.commit()?;
        }

        let n = self.conn.execute(
            "DELETE FROM file_inventory
             WHERE repo = ?1
               AND path NOT IN (SELECT path FROM _inv_live_paths)",
            rusqlite::params![repo],
        )?;
        Ok(n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::testutil::test_db;

    fn entry(repo: &str, path: &str, ext: Option<&str>, lang: Option<&str>) -> FileInventoryEntry {
        FileInventoryEntry {
            repo: repo.to_string(),
            path: path.to_string(),
            ext: ext.map(|s| s.to_string()),
            lang: lang.map(|s| s.to_string()),
            size: 100,
            mtime: "2026-07-01T00:00:00+00:00".to_string(),
            symbol_count: 0,
        }
    }

    #[test]
    fn upsert_and_list_round_trip() {
        let db = test_db();
        let entries = vec![
            entry("myrepo", "src/main.rs", Some("rs"), Some("rust")),
            entry("myrepo", "README.md", Some("md"), None),
        ];
        db.upsert_file_inventory(&entries).unwrap();

        let got = db
            .list_file_inventory(&InventoryFilter {
                repo: Some("myrepo"),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(got.len(), 2);
        // Ordered by path: README.md before src/main.rs.
        assert_eq!(got[0].path, "README.md");
        assert_eq!(got[0].lang, None);
        assert_eq!(got[1].path, "src/main.rs");
        assert_eq!(got[1].lang.as_deref(), Some("rust"));
    }

    #[test]
    fn sh_and_md_files_have_no_lang() {
        let db = test_db();
        let entries = vec![
            entry("r", "scripts/deploy.sh", Some("sh"), None),
            entry("r", "docs/guide.md", Some("md"), None),
        ];
        db.upsert_file_inventory(&entries).unwrap();

        let got = db
            .list_file_inventory(&InventoryFilter {
                repo: Some("r"),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(got.len(), 2);
        for e in &got {
            assert!(e.lang.is_none(), "{} should have lang=None", e.path);
        }
    }

    #[test]
    fn upsert_is_idempotent() {
        let db = test_db();
        let mut e = entry("r", "src/lib.rs", Some("rs"), Some("rust"));
        db.upsert_file_inventory(&[e.clone()]).unwrap();

        // Simulate a re-index: same path, updated metadata.
        e.size = 9999;
        e.mtime = "2026-07-02T00:00:00+00:00".to_string();
        db.upsert_file_inventory(&[e]).unwrap();

        let got = db
            .list_file_inventory(&InventoryFilter {
                repo: Some("r"),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(got.len(), 1, "re-index must not duplicate the row");
        assert_eq!(got[0].size, 9999);
        assert_eq!(got[0].mtime, "2026-07-02T00:00:00+00:00");
    }

    #[test]
    fn prune_removes_deleted_files() {
        let db = test_db();
        let entries = vec![
            entry("r", "a.rs", Some("rs"), Some("rust")),
            entry("r", "b.rs", Some("rs"), Some("rust")),
            entry("r", "c.rs", Some("rs"), Some("rust")),
        ];
        db.upsert_file_inventory(&entries).unwrap();

        let live: Vec<String> = vec!["a.rs".to_string(), "c.rs".to_string()];
        let pruned = db.prune_file_inventory("r", &live).unwrap();
        assert_eq!(pruned, 1, "one stale row should have been removed");

        let got = db
            .list_file_inventory(&InventoryFilter {
                repo: Some("r"),
                ..Default::default()
            })
            .unwrap();
        let paths: Vec<&str> = got.iter().map(|e| e.path.as_str()).collect();
        assert!(!paths.contains(&"b.rs"), "b.rs should have been pruned");
        assert!(paths.contains(&"a.rs"));
        assert!(paths.contains(&"c.rs"));
    }

    #[test]
    fn prune_empty_live_paths_wipes_repo() {
        let db = test_db();
        db.upsert_file_inventory(&[entry("r", "a.rs", Some("rs"), Some("rust"))])
            .unwrap();

        let pruned = db.prune_file_inventory("r", &[]).unwrap();
        assert_eq!(pruned, 1);

        let got = db
            .list_file_inventory(&InventoryFilter {
                repo: Some("r"),
                ..Default::default()
            })
            .unwrap();
        assert!(got.is_empty());
    }

    #[test]
    fn list_filtered_by_ext() {
        let db = test_db();
        let entries = vec![
            entry("r", "a.rs", Some("rs"), Some("rust")),
            entry("r", "b.md", Some("md"), None),
            entry("r", "c.rs", Some("rs"), Some("rust")),
        ];
        db.upsert_file_inventory(&entries).unwrap();

        let got = db
            .list_file_inventory(&InventoryFilter {
                repo: Some("r"),
                ext: Some("rs"),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(got.len(), 2);
        assert!(got.iter().all(|e| e.ext.as_deref() == Some("rs")));
    }

    #[test]
    fn list_scoped_to_repo() {
        let db = test_db();
        db.upsert_file_inventory(&[
            entry("alpha", "a.rs", Some("rs"), Some("rust")),
            entry("beta", "b.rs", Some("rs"), Some("rust")),
        ])
        .unwrap();

        let got = db
            .list_file_inventory(&InventoryFilter {
                repo: Some("alpha"),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].repo, "alpha");
    }
}
