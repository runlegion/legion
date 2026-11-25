//! CSS symbols: one row per class-selector or `--custom-property` definition
//! found by `css::extract_css_symbols` (#711).
//!
//! `css_symbols` is the substrate `sym` uses to answer "where is `.foo` /
//! `--token` defined" for the design system -- the question the rafters
//! agent's dynamically-composed-class bugs and token work routes through.
//! Mirrors `module_edges`'s persistence shape (#710): a full re-walk deletes
//! and re-inserts a file's rows in one transaction, so a file that loses a
//! definition does not leave a stale row behind.

use rusqlite::Connection;

use super::Database;
use crate::css::{CssSymbol, CssSymbolKind};
use crate::error::Result;

/// Create the `css_symbols` table and its lookup index.
///
/// Called from `init_schema` inside the schema-creation transaction.
/// Idempotent via `IF NOT EXISTS`.
///
/// No single-column primary key fits: the same class or custom property can
/// legitimately be (re)defined at more than one line in the same file (a
/// second `.foo { }` block, or `@theme` blocks split across `@layer`
/// sections), so the natural key is `(repo, path, kind, name, line)`.
pub(super) fn create_tables(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS css_symbols (
            repo TEXT NOT NULL,
            path TEXT NOT NULL,
            kind TEXT NOT NULL,
            name TEXT NOT NULL,
            line INTEGER NOT NULL,
            PRIMARY KEY (repo, path, kind, name, line)
        );
        CREATE INDEX IF NOT EXISTS idx_css_symbols_repo_kind_name
            ON css_symbols(repo, kind, name);",
    )?;
    Ok(())
}

fn kind_to_str(kind: CssSymbolKind) -> &'static str {
    kind.as_str()
}

fn kind_from_str(s: &str) -> rusqlite::Result<CssSymbolKind> {
    match s {
        "class" => Ok(CssSymbolKind::Class),
        "custom-property" => Ok(CssSymbolKind::CustomProperty),
        other => Err(rusqlite::Error::InvalidColumnType(
            2,
            format!("css_symbols.kind: unknown value '{other}'"),
            rusqlite::types::Type::Text,
        )),
    }
}

fn row_to_symbol(row: &rusqlite::Row<'_>) -> rusqlite::Result<CssSymbol> {
    let kind_str: String = row.get(2)?;
    Ok(CssSymbol {
        kind: kind_from_str(&kind_str)?,
        path: row.get(1)?,
        name: row.get(3)?,
        line: row.get::<_, i64>(4)? as u32,
    })
}

impl Database {
    /// Replace CSS symbols for a batch of just-(re)parsed files, keyed on
    /// `(repo, path)`.
    ///
    /// `parsed_paths` must list every file whose symbols were (re)computed
    /// this call -- including one that now yields zero symbols (its last
    /// rule was just deleted). Every existing row for each of those files is
    /// deleted before `symbols` is inserted, in the same transaction --
    /// mirrors `upsert_module_edges`'s reasoning: a file that shrinks to
    /// zero symbols contributes no rows to `symbols` at all, so nothing in
    /// the symbol list would point back at it to clear its now-stale rows.
    pub fn upsert_css_symbols(
        &self,
        repo: &str,
        parsed_paths: &[&str],
        symbols: &[CssSymbol],
    ) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        {
            let mut delete_stmt =
                tx.prepare("DELETE FROM css_symbols WHERE repo = ?1 AND path = ?2")?;
            for path in parsed_paths {
                delete_stmt.execute(rusqlite::params![repo, path])?;
            }

            let mut insert_stmt = tx.prepare(
                "INSERT INTO css_symbols (repo, path, kind, name, line)
                 VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(repo, path, kind, name, line) DO NOTHING",
            )?;
            for symbol in symbols {
                insert_stmt.execute(rusqlite::params![
                    repo,
                    &symbol.path,
                    kind_to_str(symbol.kind),
                    &symbol.name,
                    symbol.line as i64,
                ])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Return every symbol definition named `name` -- "where is `.foo` /
    /// `--token` defined" -- optionally scoped to a single repo, ordered by
    /// `(repo, path, line)` for stable output.
    ///
    /// Wired to `sym def --lang css` (#744): the CLI resolves the repo
    /// scope itself (looping per repo when `--repo` is omitted, since
    /// `CssSymbol` carries no `repo` column to attribute a cross-repo row
    /// back to its owner) rather than always calling this with `repo:
    /// None`; that call shape is still exercised directly below by
    /// `find_without_repo_searches_across_repos`.
    pub fn find_css_symbol(&self, repo: Option<&str>, name: &str) -> Result<Vec<CssSymbol>> {
        if let Some(repo) = repo {
            let mut stmt = self.conn.prepare(
                "SELECT repo, path, kind, name, line
                 FROM css_symbols
                 WHERE repo = ?1 AND name = ?2
                 ORDER BY repo, path, line",
            )?;
            let rows = stmt
                .query_map(rusqlite::params![repo, name], row_to_symbol)?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            Ok(rows)
        } else {
            let mut stmt = self.conn.prepare(
                "SELECT repo, path, kind, name, line
                 FROM css_symbols
                 WHERE name = ?1
                 ORDER BY repo, path, line",
            )?;
            let rows = stmt
                .query_map(rusqlite::params![name], row_to_symbol)?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            Ok(rows)
        }
    }

    /// Enumerate every CSS symbol for `repo` (or every repo with rows, if
    /// `None`), optionally filtered by `kind` and/or `file`. `file` mirrors
    /// `query_symbols`'s `--file` semantics for `sym list` (exact
    /// repo-relative path or a path suffix), not a substring match anywhere
    /// in the path. Ordered by `(repo, path, line)`, matching
    /// `find_css_symbol`'s own ordering.
    ///
    /// `find_css_symbol` cannot serve `sym list --lang css` directly: it is
    /// a point lookup keyed on an exact `name` (`WHERE name = ?1`), and
    /// `sym list` is an enumeration with no name argument at all -- the same
    /// def-vs-list split `sym::query_definitions` vs `sym::query_symbols`
    /// already has for SCIP. This is the `list` half.
    ///
    /// Like `find_css_symbol`, a `None` repo returns rows with no repo
    /// attribution (`CssSymbol` has no `repo` column) -- `sym list --lang
    /// css`'s CLI handler resolves the repo scope itself and calls this
    /// once per repo so it can tag each hit.
    pub fn list_css_symbols(
        &self,
        repo: Option<&str>,
        kind: Option<CssSymbolKind>,
        file: Option<&str>,
    ) -> Result<Vec<CssSymbol>> {
        let mut sql = String::from(
            "SELECT repo, path, kind, name, line
             FROM css_symbols
             WHERE 1=1",
        );
        let mut params: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
        if let Some(r) = repo {
            sql.push_str(" AND repo = ?");
            params.push(Box::new(r.to_string()));
        }
        if let Some(k) = kind {
            sql.push_str(" AND kind = ?");
            params.push(Box::new(kind_to_str(k).to_string()));
        }
        sql.push_str(" ORDER BY repo, path, line");

        let mut stmt = self.conn.prepare(&sql)?;
        let param_refs: Vec<&dyn rusqlite::ToSql> = params.iter().map(|p| p.as_ref()).collect();
        let mut rows = stmt
            .query_map(rusqlite::params_from_iter(param_refs), row_to_symbol)?
            .collect::<std::result::Result<Vec<_>, _>>()?;

        if let Some(f) = file {
            rows.retain(|s| s.path == f || s.path.ends_with(f));
        }

        Ok(rows)
    }

    /// Delete CSS symbols for `repo` whose file is not in `live_paths`.
    ///
    /// Mirrors `prune_module_edges`: call after `upsert_css_symbols` on a
    /// full repo re-walk so symbols from deleted/renamed files do not
    /// linger. When `live_paths` is empty every row for the repo is removed.
    pub fn prune_css_symbols(&self, repo: &str, live_paths: &[&str]) -> Result<usize> {
        if live_paths.is_empty() {
            let n = self.conn.execute(
                "DELETE FROM css_symbols WHERE repo = ?1",
                rusqlite::params![repo],
            )?;
            return Ok(n);
        }

        self.conn.execute_batch(
            "CREATE TEMP TABLE IF NOT EXISTS _css_symbols_live_paths (path TEXT PRIMARY KEY)",
        )?;
        self.conn
            .execute_batch("DELETE FROM _css_symbols_live_paths")?;

        {
            let tx = self.conn.unchecked_transaction()?;
            {
                let mut insert =
                    tx.prepare("INSERT OR IGNORE INTO _css_symbols_live_paths (path) VALUES (?1)")?;
                for p in live_paths {
                    insert.execute(rusqlite::params![p])?;
                }
            }
            tx.commit()?;
        }

        let n = self.conn.execute(
            "DELETE FROM css_symbols
             WHERE repo = ?1
               AND path NOT IN (SELECT path FROM _css_symbols_live_paths)",
            rusqlite::params![repo],
        )?;
        Ok(n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::testutil::test_db;

    fn symbol(kind: CssSymbolKind, name: &str, path: &str, line: u32) -> CssSymbol {
        CssSymbol {
            kind,
            name: name.to_string(),
            path: path.to_string(),
            line,
        }
    }

    #[test]
    fn upsert_and_find_round_trip() {
        let db = test_db();
        let symbols = vec![
            symbol(CssSymbolKind::Class, ".foo", "a.css", 1),
            symbol(CssSymbolKind::CustomProperty, "--brand", "a.css", 5),
        ];
        db.upsert_css_symbols("r", &["a.css"], &symbols).unwrap();

        let got = db.find_css_symbol(Some("r"), ".foo").unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].kind, CssSymbolKind::Class);
        assert_eq!(got[0].path, "a.css");
        assert_eq!(got[0].line, 1);
    }

    #[test]
    fn find_without_repo_searches_across_repos() {
        let db = test_db();
        db.upsert_css_symbols(
            "alpha",
            &["a.css"],
            &[symbol(CssSymbolKind::Class, ".shared", "a.css", 1)],
        )
        .unwrap();
        db.upsert_css_symbols(
            "beta",
            &["b.css"],
            &[symbol(CssSymbolKind::Class, ".shared", "b.css", 2)],
        )
        .unwrap();

        let got = db.find_css_symbol(None, ".shared").unwrap();
        assert_eq!(got.len(), 2, "cross-repo search must find both");
    }

    #[test]
    fn reindex_drops_a_removed_symbol_from_a_still_live_file() {
        let db = test_db();
        db.upsert_css_symbols(
            "r",
            &["a.css"],
            &[
                symbol(CssSymbolKind::Class, ".foo", "a.css", 1),
                symbol(CssSymbolKind::Class, ".bar", "a.css", 2),
            ],
        )
        .unwrap();

        // Re-index after the file drops `.bar`. The file is still live
        // (still passed in `parsed_paths`), so `prune_css_symbols` never
        // sees it -- the stale `.bar` row must be dropped by the upsert
        // itself.
        db.upsert_css_symbols(
            "r",
            &["a.css"],
            &[symbol(CssSymbolKind::Class, ".foo", "a.css", 1)],
        )
        .unwrap();

        let got = db.find_css_symbol(Some("r"), ".bar").unwrap();
        assert!(
            got.is_empty(),
            "the removed class must not survive re-indexing"
        );
        assert_eq!(db.find_css_symbol(Some("r"), ".foo").unwrap().len(), 1);
    }

    #[test]
    fn reindex_clears_all_symbols_when_a_live_file_drops_every_rule() {
        let db = test_db();
        db.upsert_css_symbols(
            "r",
            &["a.css"],
            &[symbol(CssSymbolKind::Class, ".foo", "a.css", 1)],
        )
        .unwrap();

        // The file still exists (still passed as a parsed path) but now
        // yields zero symbols.
        db.upsert_css_symbols("r", &["a.css"], &[]).unwrap();

        assert!(db.find_css_symbol(Some("r"), ".foo").unwrap().is_empty());
    }

    #[test]
    fn prune_removes_symbols_for_deleted_files() {
        let db = test_db();
        db.upsert_css_symbols(
            "r",
            &["a.css", "gone.css"],
            &[
                symbol(CssSymbolKind::Class, ".foo", "a.css", 1),
                symbol(CssSymbolKind::Class, ".gone", "gone.css", 1),
            ],
        )
        .unwrap();

        let pruned = db.prune_css_symbols("r", &["a.css"]).unwrap();
        assert_eq!(pruned, 1);
        assert!(db.find_css_symbol(Some("r"), ".gone").unwrap().is_empty());
        assert_eq!(db.find_css_symbol(Some("r"), ".foo").unwrap().len(), 1);
    }

    #[test]
    fn prune_empty_live_paths_wipes_repo() {
        let db = test_db();
        db.upsert_css_symbols(
            "r",
            &["a.css"],
            &[symbol(CssSymbolKind::Class, ".foo", "a.css", 1)],
        )
        .unwrap();

        let pruned = db.prune_css_symbols("r", &[]).unwrap();
        assert_eq!(pruned, 1);
        assert!(db.find_css_symbol(Some("r"), ".foo").unwrap().is_empty());
    }

    #[test]
    fn same_name_at_different_lines_in_the_same_file_are_both_kept() {
        // A class can legitimately be (re)defined more than once in the
        // same compiled sheet.
        let db = test_db();
        db.upsert_css_symbols(
            "r",
            &["a.css"],
            &[
                symbol(CssSymbolKind::Class, ".foo", "a.css", 1),
                symbol(CssSymbolKind::Class, ".foo", "a.css", 20),
            ],
        )
        .unwrap();

        let got = db.find_css_symbol(Some("r"), ".foo").unwrap();
        assert_eq!(got.len(), 2, "both definition sites must be kept");
    }

    #[test]
    fn list_css_symbols_enumerates_all_rows_for_a_repo() {
        let db = test_db();
        db.upsert_css_symbols(
            "r",
            &["a.css"],
            &[
                symbol(CssSymbolKind::Class, ".foo", "a.css", 1),
                symbol(CssSymbolKind::CustomProperty, "--brand", "a.css", 5),
            ],
        )
        .unwrap();

        let got = db.list_css_symbols(Some("r"), None, None).unwrap();
        assert_eq!(got.len(), 2);
    }

    #[test]
    fn list_css_symbols_filters_by_kind() {
        let db = test_db();
        db.upsert_css_symbols(
            "r",
            &["a.css"],
            &[
                symbol(CssSymbolKind::Class, ".foo", "a.css", 1),
                symbol(CssSymbolKind::CustomProperty, "--brand", "a.css", 5),
            ],
        )
        .unwrap();

        let classes = db
            .list_css_symbols(Some("r"), Some(CssSymbolKind::Class), None)
            .unwrap();
        assert_eq!(classes.len(), 1);
        assert_eq!(classes[0].kind, CssSymbolKind::Class);

        let props = db
            .list_css_symbols(Some("r"), Some(CssSymbolKind::CustomProperty), None)
            .unwrap();
        assert_eq!(props.len(), 1);
        assert_eq!(props[0].kind, CssSymbolKind::CustomProperty);
    }

    #[test]
    fn list_css_symbols_filters_by_file_exact_or_suffix() {
        let db = test_db();
        db.upsert_css_symbols(
            "r",
            &["src/a.css", "src/b.css"],
            &[
                symbol(CssSymbolKind::Class, ".foo", "src/a.css", 1),
                symbol(CssSymbolKind::Class, ".bar", "src/b.css", 1),
            ],
        )
        .unwrap();

        let exact = db
            .list_css_symbols(Some("r"), None, Some("src/a.css"))
            .unwrap();
        assert_eq!(exact.len(), 1);
        assert_eq!(exact[0].name, ".foo");

        let suffix = db.list_css_symbols(Some("r"), None, Some("a.css")).unwrap();
        assert_eq!(suffix.len(), 1);
        assert_eq!(suffix[0].name, ".foo");
    }

    #[test]
    fn list_css_symbols_without_repo_searches_across_repos() {
        let db = test_db();
        db.upsert_css_symbols(
            "alpha",
            &["a.css"],
            &[symbol(CssSymbolKind::Class, ".shared", "a.css", 1)],
        )
        .unwrap();
        db.upsert_css_symbols(
            "beta",
            &["b.css"],
            &[symbol(CssSymbolKind::Class, ".shared", "b.css", 2)],
        )
        .unwrap();

        let got = db.list_css_symbols(None, None, None).unwrap();
        assert_eq!(got.len(), 2, "cross-repo enumeration must find both");
    }

    #[test]
    fn list_css_symbols_empty_result_is_not_an_error() {
        let db = test_db();
        db.upsert_css_symbols(
            "r",
            &["a.css"],
            &[symbol(CssSymbolKind::Class, ".foo", "a.css", 1)],
        )
        .unwrap();

        let got = db
            .list_css_symbols(Some("r"), Some(CssSymbolKind::CustomProperty), None)
            .unwrap();
        assert!(got.is_empty());
    }
}
