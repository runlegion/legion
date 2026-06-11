//! SCIP indexes: stored protobuf blobs for code intelligence (#278).
//! Owns the `scip_indexes` DDL.

use rusqlite::{Connection, OptionalExtension};

use super::Database;
use crate::error::Result;

/// `scip_indexes` table (#278).
pub(super) fn create_tables(conn: &Connection) -> Result<()> {
    // Migration 17: SCIP indexes for code intelligence (#278).
    // One active row per (repo, lang) holds the protobuf blob legion
    // queries against. content_hash is SHA-256 hex of the blob; an
    // upsert with an unchanged hash short-circuits to bumping
    // updated_at without rewriting the blob bytes.
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS scip_indexes (
                id TEXT PRIMARY KEY,
                repo TEXT NOT NULL,
                lang TEXT NOT NULL,
                content_hash TEXT NOT NULL,
                blob BLOB NOT NULL,
                updated_at TEXT NOT NULL,
                deleted_at TEXT
            );
            CREATE UNIQUE INDEX IF NOT EXISTS idx_scip_indexes_repo_lang_live
                ON scip_indexes(repo, lang) WHERE deleted_at IS NULL;
            CREATE INDEX IF NOT EXISTS idx_scip_indexes_repo
                ON scip_indexes(repo) WHERE deleted_at IS NULL;",
    )?;
    Ok(())
}

impl Database {
    /// Write or overwrite a SCIP index row identified by (repo, lang).
    /// Idempotent on `content_hash`: when the stored hash matches the
    /// incoming one, only `updated_at` is bumped. Otherwise the blob,
    /// hash, and timestamp are all rewritten.
    pub fn upsert_scip_index(&self, index: &crate::scip::ScipIndex) -> Result<()> {
        let existing: Option<(String, String)> = self
            .conn
            .query_row(
                "SELECT id, content_hash FROM scip_indexes
                 WHERE repo = ?1 AND lang = ?2 AND deleted_at IS NULL",
                rusqlite::params![&index.repo, &index.lang],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;

        match existing {
            Some((id, hash)) if hash == index.content_hash => {
                self.conn.execute(
                    "UPDATE scip_indexes SET updated_at = ?1 WHERE id = ?2",
                    rusqlite::params![&index.updated_at, &id],
                )?;
            }
            Some((id, _)) => {
                self.conn.execute(
                    "UPDATE scip_indexes
                     SET content_hash = ?1, blob = ?2, updated_at = ?3
                     WHERE id = ?4",
                    rusqlite::params![&index.content_hash, &index.blob, &index.updated_at, &id],
                )?;
            }
            None => {
                self.conn.execute(
                    "INSERT INTO scip_indexes
                     (id, repo, lang, content_hash, blob, updated_at, deleted_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, NULL)",
                    rusqlite::params![
                        &index.id,
                        &index.repo,
                        &index.lang,
                        &index.content_hash,
                        &index.blob,
                        &index.updated_at,
                    ],
                )?;
            }
        }
        Ok(())
    }

    /// Retrieve the active stored index for (repo, lang).
    /// Read path consumed by the SCIP query CLI in #282.
    #[allow(dead_code)]
    pub fn get_scip_index(&self, repo: &str, lang: &str) -> Result<Option<crate::scip::ScipIndex>> {
        let row = self
            .conn
            .query_row(
                "SELECT id, repo, lang, content_hash, blob, updated_at, deleted_at
                 FROM scip_indexes
                 WHERE repo = ?1 AND lang = ?2 AND deleted_at IS NULL",
                rusqlite::params![repo, lang],
                |row| {
                    Ok(crate::scip::ScipIndex {
                        id: row.get(0)?,
                        repo: row.get(1)?,
                        lang: row.get(2)?,
                        content_hash: row.get(3)?,
                        blob: row.get(4)?,
                        updated_at: row.get(5)?,
                        deleted_at: row.get(6)?,
                    })
                },
            )
            .optional()?;
        Ok(row)
    }

    /// List non-deleted SCIP indexes filtered by optional repo and language.
    /// `None` on either field disables that filter; `(None, None)` returns
    /// every live index. Used by `legion sym` query dispatch (#282) so a
    /// single call covers `--repo`/`--lang` scoping.
    pub fn list_scip_indexes_filtered(
        &self,
        repo: Option<&str>,
        lang: Option<&str>,
    ) -> Result<Vec<crate::scip::ScipIndex>> {
        let mut sql = String::from(
            "SELECT id, repo, lang, content_hash, blob, updated_at, deleted_at
             FROM scip_indexes
             WHERE deleted_at IS NULL",
        );
        let mut params: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
        if let Some(r) = repo {
            sql.push_str(" AND repo = ?");
            params.push(Box::new(r.to_string()));
        }
        if let Some(l) = lang {
            sql.push_str(" AND lang = ?");
            params.push(Box::new(l.to_string()));
        }
        sql.push_str(" ORDER BY repo, lang");

        let mut stmt = self.conn.prepare(&sql)?;
        let param_refs: Vec<&dyn rusqlite::ToSql> = params.iter().map(|p| p.as_ref()).collect();
        let rows = stmt
            .query_map(rusqlite::params_from_iter(param_refs), |row| {
                Ok(crate::scip::ScipIndex {
                    id: row.get(0)?,
                    repo: row.get(1)?,
                    lang: row.get(2)?,
                    content_hash: row.get(3)?,
                    blob: row.get(4)?,
                    updated_at: row.get(5)?,
                    deleted_at: row.get(6)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// List all non-deleted SCIP indexes for a repo.
    /// Read path consumed by `legion consult --symbol` in #285.
    #[allow(dead_code)]
    pub fn list_scip_indexes(&self, repo: &str) -> Result<Vec<crate::scip::ScipIndex>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, repo, lang, content_hash, blob, updated_at, deleted_at
             FROM scip_indexes
             WHERE repo = ?1 AND deleted_at IS NULL
             ORDER BY lang",
        )?;
        let rows = stmt
            .query_map(rusqlite::params![repo], |row| {
                Ok(crate::scip::ScipIndex {
                    id: row.get(0)?,
                    repo: row.get(1)?,
                    lang: row.get(2)?,
                    content_hash: row.get(3)?,
                    blob: row.get(4)?,
                    updated_at: row.get(5)?,
                    deleted_at: row.get(6)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }
}

#[cfg(test)]
mod tests {
    use crate::db::testutil::test_db;

    fn make_scip(repo: &str, lang: &str, blob: &[u8]) -> crate::scip::ScipIndex {
        crate::scip::ScipIndex {
            id: uuid::Uuid::now_v7().to_string(),
            repo: repo.to_string(),
            lang: lang.to_string(),
            content_hash: crate::scip::content_hash(blob),
            blob: blob.to_vec(),
            updated_at: "2026-04-28T00:00:00Z".to_string(),
            deleted_at: None,
        }
    }

    #[test]
    fn scip_upsert_then_get_round_trip() {
        let db = test_db();
        let idx = make_scip("legion", "rust", b"hello scip");
        db.upsert_scip_index(&idx).unwrap();

        let got = db.get_scip_index("legion", "rust").unwrap().unwrap();
        assert_eq!(got.content_hash, idx.content_hash);
        assert_eq!(got.blob, idx.blob);
        assert_eq!(got.lang, "rust");
    }

    #[test]
    fn scip_upsert_idempotent_on_unchanged_hash() {
        let db = test_db();
        let mut idx = make_scip("legion", "rust", b"same bytes");
        db.upsert_scip_index(&idx).unwrap();
        let original_id = db.get_scip_index("legion", "rust").unwrap().unwrap().id;

        // Second call with identical content_hash -- bumps updated_at
        // but does not rewrite the row identity.
        idx.updated_at = "2026-04-29T00:00:00Z".to_string();
        db.upsert_scip_index(&idx).unwrap();

        let after = db.get_scip_index("legion", "rust").unwrap().unwrap();
        assert_eq!(after.id, original_id, "upsert must not replace the row");
        assert_eq!(after.updated_at, "2026-04-29T00:00:00Z");
    }

    #[test]
    fn scip_upsert_overwrites_blob_when_hash_changes() {
        let db = test_db();
        db.upsert_scip_index(&make_scip("legion", "rust", b"v1"))
            .unwrap();
        db.upsert_scip_index(&make_scip("legion", "rust", b"v2"))
            .unwrap();

        let got = db.get_scip_index("legion", "rust").unwrap().unwrap();
        assert_eq!(got.blob, b"v2");
    }

    #[test]
    fn scip_list_returns_all_languages_for_repo() {
        let db = test_db();
        db.upsert_scip_index(&make_scip("legion", "rust", b"r"))
            .unwrap();
        db.upsert_scip_index(&make_scip("legion", "ts", b"t"))
            .unwrap();
        db.upsert_scip_index(&make_scip("other", "rust", b"x"))
            .unwrap();

        let mut langs: Vec<String> = db
            .list_scip_indexes("legion")
            .unwrap()
            .into_iter()
            .map(|i| i.lang)
            .collect();
        langs.sort();
        assert_eq!(langs, vec!["rust".to_string(), "ts".to_string()]);
    }

    #[test]
    fn scip_soft_deleted_row_excluded_from_list() {
        let db = test_db();
        let idx = make_scip("legion", "rust", b"x");
        db.upsert_scip_index(&idx).unwrap();
        db.conn
            .execute(
                "UPDATE scip_indexes SET deleted_at = '2026-04-28T00:00:00Z' WHERE repo = 'legion'",
                [],
            )
            .unwrap();

        assert!(
            db.list_scip_indexes("legion").unwrap().is_empty(),
            "soft-deleted rows must not appear in list"
        );
        assert!(
            db.get_scip_index("legion", "rust").unwrap().is_none(),
            "soft-deleted rows must not appear in get"
        );

        // Raw row still present in the table.
        let count: i64 = db
            .conn
            .query_row("SELECT COUNT(*) FROM scip_indexes", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 1);
    }
}
