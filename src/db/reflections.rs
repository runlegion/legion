//! Reflections: the core memory rows, embeddings, learning chains, and
//! domain-root queries. Owns the `reflections` table DDL and its column
//! migrations (bullpen lifecycle columns migrate in `super::board`).

use chrono::Utc;
use rusqlite::{Connection, OptionalExtension};
use uuid::Uuid;

use super::Database;
use super::board::compute_post_ttl;
use crate::error::{LegionError, Result};

/// A reflection row returned with its embedding blob for dedupe checks.
///
/// Tuple fields: (id, embedding_bytes, text, created_at).
pub type ReflectionWithEmbedding = (String, Vec<u8>, String, String);

/// A single stored reflection tied to a repository.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Reflection {
    pub id: String,
    pub repo: String,
    pub text: String,
    pub created_at: String,
    pub updated_at: Option<String>,
    pub audience: String,
    // Phase 2.0: Synapse metadata
    pub domain: Option<String>,
    pub tags: Option<String>,
    pub recall_count: i64,
    pub last_recalled_at: Option<String>,
    pub parent_id: Option<String>,
}

/// The canonical column list for every reflections SELECT consumed by
/// [`map_reflection_row`]. The order must match the positional `row.get(n)`
/// reads in that function exactly -- omitting or reordering a column shifts
/// every subsequent index and fails at runtime, not compile time (#606).
pub(crate) const REFLECTION_COLUMNS: &str = "id, repo, text, created_at, updated_at, audience, domain, \
     tags, recall_count, last_recalled_at, parent_id";

/// Map a database row to a Reflection struct.
///
/// Shared by all queries that select [`REFLECTION_COLUMNS`] positionally.
pub(crate) fn map_reflection_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Reflection> {
    Ok(Reflection {
        id: row.get(0)?,
        repo: row.get(1)?,
        text: row.get(2)?,
        created_at: row.get(3)?,
        updated_at: row.get(4)?,
        audience: row.get(5)?,
        domain: row.get(6)?,
        tags: row.get(7)?,
        recall_count: row.get(8)?,
        last_recalled_at: row.get(9)?,
        parent_id: row.get(10)?,
    })
}

/// Optional metadata for a new reflection (Phase 2.0 Synapse fields).
#[derive(Default)]
pub struct ReflectionMeta {
    pub domain: Option<String>,
    pub tags: Option<String>,
    pub parent_id: Option<String>,
}

/// Result of [`Database::swap_identity_root`]: the ids removed, the new
/// root, and any chained children inserted after it (in chain order).
pub struct IdentitySwapResult {
    pub deleted_ids: Vec<String>,
    pub root: Reflection,
    pub children: Vec<Reflection>,
}

/// Base `reflections` table and the indexes over its original columns.
pub(super) fn create_tables(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS reflections (
                id TEXT PRIMARY KEY,
                repo TEXT NOT NULL,
                text TEXT NOT NULL,
                created_at TEXT NOT NULL,
                embedding BLOB
            );
            CREATE INDEX IF NOT EXISTS idx_reflections_repo ON reflections(repo);
            CREATE INDEX IF NOT EXISTS idx_reflections_created ON reflections(created_at);",
    )?;
    Ok(())
}

/// Column migrations for `reflections`, in their original patch order.
/// The bullpen lifecycle columns (decay #376, resolution #362) are board
/// domain and migrate in `super::board::migrate`.
pub(super) fn migrate(conn: &Connection) -> Result<()> {
    // Migration 1: add audience column (board_reads lives in super::board).
    // Only run when the column does not yet exist.
    if !Database::has_column(conn, "reflections", "audience")? {
        conn.execute_batch(
            "ALTER TABLE reflections ADD COLUMN audience TEXT NOT NULL DEFAULT 'self';",
        )?;
    }

    // Migration 2: Phase 2.0 Synapse metadata columns.
    if !Database::has_column(conn, "reflections", "domain")? {
        conn.execute_batch("ALTER TABLE reflections ADD COLUMN domain TEXT;")?;
    }
    if !Database::has_column(conn, "reflections", "tags")? {
        conn.execute_batch("ALTER TABLE reflections ADD COLUMN tags TEXT;")?;
    }
    if !Database::has_column(conn, "reflections", "recall_count")? {
        conn.execute_batch(
            "ALTER TABLE reflections ADD COLUMN recall_count INTEGER NOT NULL DEFAULT 0;",
        )?;
    }
    if !Database::has_column(conn, "reflections", "last_recalled_at")? {
        conn.execute_batch("ALTER TABLE reflections ADD COLUMN last_recalled_at TEXT;")?;
    }
    if !Database::has_column(conn, "reflections", "parent_id")? {
        conn.execute_batch("ALTER TABLE reflections ADD COLUMN parent_id TEXT;")?;
    }

    // Migration 6: Add handled_at column for watch auto-wake tracking.
    if !Database::has_column(conn, "reflections", "handled_at")? {
        conn.execute_batch("ALTER TABLE reflections ADD COLUMN handled_at TEXT;")?;
    }

    // Migration 10: Bullpen archive -- nullable archived_at on reflections (#168).
    if !Database::has_column(conn, "reflections", "archived_at")? {
        conn.execute_batch("ALTER TABLE reflections ADD COLUMN archived_at TEXT;")?;
    }
    conn.execute_batch(
        "CREATE INDEX IF NOT EXISTS idx_reflections_archived \
             ON reflections(archived_at, created_at);",
    )?;

    // Migration 13: Soft delete support for multi-node sync (#245).
    // Adds deleted_at column to syncable tables. Rows with deleted_at set
    // are excluded from normal queries but included in sync deltas.
    if !Database::has_column(conn, "reflections", "deleted_at")? {
        conn.execute_batch("ALTER TABLE reflections ADD COLUMN deleted_at TEXT;")?;
    }

    // Migration 14: Add updated_at for LWW conflict resolution (#255).
    // Required for multi-node sync to determine which version wins when
    // the same row is modified on different nodes.
    if !Database::has_column(conn, "reflections", "updated_at")? {
        conn.execute_batch(
            "ALTER TABLE reflections ADD COLUMN updated_at TEXT;
                 UPDATE reflections SET updated_at = created_at WHERE updated_at IS NULL;",
        )?;
    }

    // Migration 15: Partial indexes for soft-deleted rows (#256).
    // Most queries filter by deleted_at IS NULL. Partial indexes exclude
    // tombstones, reducing index size and improving query performance.
    conn.execute_batch(
        "CREATE INDEX IF NOT EXISTS idx_reflections_repo_live \
                 ON reflections(repo) WHERE deleted_at IS NULL;
             CREATE INDEX IF NOT EXISTS idx_reflections_audience_live \
                 ON reflections(audience, created_at) WHERE deleted_at IS NULL;",
    )?;
    Ok(())
}

impl Database {
    /// Insert a new reflection for the given repository.
    ///
    /// Generates a UUIDv7 id and ISO 8601 timestamp automatically.
    /// The `audience` parameter controls visibility: "self" for private
    /// reflections, "team" for bullpen posts visible to all agents.
    #[allow(dead_code)]
    pub fn insert_reflection(&self, repo: &str, text: &str, audience: &str) -> Result<Reflection> {
        self.insert_reflection_with_meta(repo, text, audience, &ReflectionMeta::default())
    }

    /// Insert a new reflection with optional Synapse metadata.
    ///
    /// Like `insert_reflection` but accepts domain, tags, and parent_id
    /// for learning chain linking and classification.
    ///
    /// Guard (#785): when `meta.domain` is `Some("identity")` and
    /// `meta.parent_id` is `None`, this is a write to the identity-root
    /// slot. At most one live identity root may exist per repo -- a
    /// second orphan root is exactly how a stray checkpoint-shaped
    /// reflection outranked a repo's real identity in the boot banner
    /// (`019f198b`). If [`live_identity_root_id`](Self::live_identity_root_id)
    /// finds one already, the insert is refused with
    /// `LegionError::IdentityRootExists` before any row is written. This
    /// check is unconditional with respect to caller intent -- it does
    /// not look at `--force` or any other flag, because every caller (CLI
    /// `reflect` today, any future MCP tool or hook-invoked call
    /// tomorrow) converges here. Bootstrap (no live root yet) and
    /// explicit chaining (`meta.parent_id = Some(_)`, e.g. `--follows`)
    /// are both unaffected.
    ///
    /// This is an application-level check-then-insert, not a database
    /// constraint: two concurrent `legion` processes racing this method
    /// for the same repo could both read "no live root" before either
    /// writes, and both succeed, producing the very state this guard
    /// exists to prevent. `legion` is a per-invocation CLI with no long-
    /// lived writer serializing access, so this window is real, not
    /// theoretical. A DB-level `UNIQUE` partial index would close it
    /// structurally, but cannot be added here: it would fail its own
    /// migration on any already-leaked database (domain=identity rows
    /// with `parent_id IS NULL` already duplicated, e.g. `019f198b`'s
    /// repo before hand-repair) -- and retroactive cleanup of existing
    /// duplicates is explicitly out of scope for this change. This guard
    /// closes the write-time gap for the overwhelmingly common
    /// single-process case; the concurrent-write race is a known,
    /// documented gap for a future change to close once a cleanup path
    /// exists.
    pub fn insert_reflection_with_meta(
        &self,
        repo: &str,
        text: &str,
        audience: &str,
        meta: &ReflectionMeta,
    ) -> Result<Reflection> {
        if meta.domain.as_deref() == Some("identity")
            && meta.parent_id.is_none()
            && let Some(existing_id) = self.live_identity_root_id(repo)?
        {
            return Err(LegionError::IdentityRootExists {
                repo: repo.to_owned(),
                existing_id,
            });
        }

        Self::insert_reflection_row(&self.conn, repo, text, audience, meta)
    }

    /// Core row-insert logic, shared by [`insert_reflection_with_meta`]
    /// (guarded, against `self.conn`) and [`swap_identity_root`] (against
    /// an in-flight transaction). Takes `&Connection` rather than `&self`
    /// so both a plain connection and a `rusqlite::Transaction` (which
    /// derefs to `Connection`) can call it. Carries no guard of its own --
    /// callers are responsible for any invariant checks before calling.
    ///
    /// [`insert_reflection_with_meta`]: Self::insert_reflection_with_meta
    /// [`swap_identity_root`]: Self::swap_identity_root
    fn insert_reflection_row(
        conn: &Connection,
        repo: &str,
        text: &str,
        audience: &str,
        meta: &ReflectionMeta,
    ) -> Result<Reflection> {
        let id = Uuid::now_v7().to_string();
        let created_at_dt = Utc::now();
        let created_at = created_at_dt.to_rfc3339();
        let (expires_at, evergreen) = compute_post_ttl(
            audience,
            meta.domain.as_deref(),
            meta.tags.as_deref(),
            text,
            created_at_dt,
        );

        conn.execute(
            "INSERT INTO reflections (id, repo, text, created_at, updated_at, audience, domain, tags, parent_id, expires_at, evergreen) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            rusqlite::params![
                &id, repo, text, &created_at, &created_at, audience,
                &meta.domain, &meta.tags, &meta.parent_id,
                expires_at, evergreen as i32,
            ],
        )?;

        Ok(Reflection {
            id,
            repo: repo.to_owned(),
            text: text.to_owned(),
            created_at: created_at.clone(),
            updated_at: Some(created_at),
            audience: audience.to_owned(),
            domain: meta.domain.clone(),
            tags: meta.tags.clone(),
            recall_count: 0,
            last_recalled_at: None,
            parent_id: meta.parent_id.clone(),
        })
    }

    /// Return the id of the repo's live identity root, if one exists.
    ///
    /// "Live" here means `domain = 'identity' AND parent_id IS NULL AND
    /// deleted_at IS NULL AND archived_at IS NULL` -- a standalone query,
    /// not a call to [`get_identity_roots`](Self::get_identity_roots),
    /// because the guard's liveness definition must include the
    /// `archived_at` filter now (an archived root must not block a new
    /// bootstrap or replacement) while the shared banner reader behind
    /// `get_identity_roots` does not filter `archived_at` yet (tracked
    /// separately as #782). This function does not wait for that fix.
    ///
    /// The asymmetry this creates -- an archived root plus a fresh root
    /// both satisfying `get_identity_roots`'s current predicate -- is not
    /// reachable through any production write path today: `legion
    /// reflect` always stores identity reflections with `audience =
    /// "self"` (hardcoded; there is no `--audience` flag), and the only
    /// writer of `archived_at` on the `reflections` table
    /// (`Database::archive_read_posts`, `src/db/board.rs`) is scoped to
    /// `audience = 'team'`. Verified in
    /// `insert_reflection_with_meta_ignores_archived_root_for_guard`,
    /// which asserts the two-root state directly rather than leaving it
    /// implicit.
    fn live_identity_root_id(&self, repo: &str) -> Result<Option<String>> {
        // ORDER BY pins which id surfaces in the IdentityRootExists error
        // message when more than one live root already exists (reachable
        // only via a guard-bypassing path like sync's apply_reflection_delta
        // -- see get_identity_roots_excludes_chain_children's raw-INSERT
        // fixture for the same scenario). Newest first, matching
        // get_domain_roots's ordering, so the id in the error is
        // deterministic rather than whichever row SQLite happens to
        // return first.
        self.conn
            .query_row(
                "SELECT id FROM reflections \
                 WHERE repo = ?1 AND domain = 'identity' AND parent_id IS NULL \
                   AND deleted_at IS NULL AND archived_at IS NULL \
                 ORDER BY created_at DESC LIMIT 1",
                [repo],
                |row| row.get(0),
            )
            .optional()
            .map_err(LegionError::Database)
    }

    /// Atomically replace a repo's identity root (and, optionally, chain
    /// it to fresh children) inside a single transaction.
    ///
    /// This is the ONLY sanctioned way to replace an existing identity
    /// root. Unlike the `legion forget --id <root>` then `legion reflect
    /// --whoami` sequence (which has a brief window with zero live
    /// roots), this method never exposes an intermediate state to a
    /// concurrent reader: either the swap fully commits (old root(s)
    /// gone, new root and any chained children in place) or it fully
    /// rolls back (old identity untouched) on any failure -- rusqlite
    /// drops an uncommitted `Transaction` with an automatic ROLLBACK, so
    /// a mid-swap error restores the prior state with no extra code here.
    ///
    /// Deletes every row matching `domain = 'identity' AND parent_id IS
    /// NULL AND deleted_at IS NULL` for `repo` -- deliberately not
    /// filtering `archived_at` the way the insert guard's
    /// [`live_identity_root_id`](Self::live_identity_root_id) does. An
    /// archived orphan left behind by a swap would still satisfy "a
    /// second domain=identity, parent_id IS NULL row" for any reader that
    /// does not yet filter `archived_at` (the banner reader, until #782),
    /// which is exactly the two-roots bug this issue closes. Swap is the
    /// explicit "replace everything" path, so it clears every *root* row,
    /// including any pre-existing leaked duplicates from before this fix
    /// landed -- the plain insert guard only ever sees (and blocks
    /// against) one at a time.
    ///
    /// Deliberately NOT cascading: only the root row(s) are deleted, not
    /// their `--follows` chain children. A replaced root's old chain
    /// children (if any) survive as now-dangling `parent_id` references,
    /// exactly like `legion forget --id <root>` already leaves them today
    /// (`delete_reflection` is a single-row delete with no cascade). This
    /// keeps swap's delete semantics consistent with the one delete
    /// primitive that already exists, rather than inventing new cascade
    /// behavior no other code path has. Full old-chain retirement is a
    /// call for whichever future feature actually builds replacement
    /// chains on top of this (`whoami --generate`, #784) to make against
    /// its own real requirements, not a guess made here.
    ///
    /// `new_root_text` becomes the new root (`parent_id = None`).
    /// `chained_texts`, if any, are inserted afterward as a chain: the
    /// first follows the new root, each subsequent one follows the last.
    pub fn swap_identity_root(
        &self,
        repo: &str,
        new_root_text: &str,
        chained_texts: &[&str],
        audience: &str,
    ) -> Result<IdentitySwapResult> {
        let tx = self.conn.unchecked_transaction()?;

        // One statement, not a SELECT-then-DELETE pair with the liveness
        // predicate copy-pasted twice: `RETURNING id` deletes every live
        // identity root for `repo` and hands back exactly which ones,
        // in one round trip, so there is only one place the predicate can
        // drift out of sync with the guard's own definition.
        let mut delete_stmt = tx.prepare(
            "DELETE FROM reflections \
             WHERE repo = ?1 AND domain = 'identity' AND parent_id IS NULL AND deleted_at IS NULL \
             RETURNING id",
        )?;
        let deleted_ids: Vec<String> = delete_stmt
            .query_map([repo], |row| row.get(0))?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(LegionError::Database)?;
        drop(delete_stmt);

        let root_meta = ReflectionMeta {
            domain: Some("identity".to_owned()),
            tags: None,
            parent_id: None,
        };
        let root = Self::insert_reflection_row(&tx, repo, new_root_text, audience, &root_meta)?;

        let mut children = Vec::with_capacity(chained_texts.len());
        let mut parent_id = root.id.clone();
        for text in chained_texts {
            let child_meta = ReflectionMeta {
                domain: Some("identity".to_owned()),
                tags: None,
                parent_id: Some(parent_id.clone()),
            };
            let child = Self::insert_reflection_row(&tx, repo, text, audience, &child_meta)?;
            parent_id = child.id.clone();
            children.push(child);
        }

        tx.commit()?;

        Ok(IdentitySwapResult {
            deleted_ids,
            root,
            children,
        })
    }

    /// Store an embedding BLOB for an existing reflection.
    pub fn store_embedding(&self, id: &str, embedding_bytes: &[u8]) -> Result<bool> {
        let now = Utc::now().to_rfc3339();
        let rows = self.conn.execute(
            "UPDATE reflections SET embedding = ?1, updated_at = ?3 WHERE id = ?2 AND deleted_at IS NULL",
            rusqlite::params![embedding_bytes, id, &now],
        )?;
        Ok(rows > 0)
    }

    /// Retrieve the embedding BLOB for a reflection, if it exists.
    pub fn get_embedding(&self, id: &str) -> Result<Option<Vec<u8>>> {
        let mut stmt = self
            .conn
            .prepare("SELECT embedding FROM reflections WHERE id = ?1 AND deleted_at IS NULL")?;
        let mut rows = stmt.query_map([id], |row| {
            let blob: Option<Vec<u8>> = row.get(0)?;
            Ok(blob)
        })?;
        match rows.next() {
            Some(row) => Ok(row?),
            None => Ok(None),
        }
    }

    /// Retrieve all reflections that have embeddings, optionally filtered by repo.
    ///
    /// Returns (id, embedding_bytes) pairs for cosine similarity search.
    /// Pass `None` for cross-repo search (consult), or `Some(repo)` for
    /// repo-scoped search (recall).
    pub fn get_embeddings(&self, repo: Option<&str>) -> Result<Vec<(String, Vec<u8>)>> {
        let map_row = |row: &rusqlite::Row<'_>| -> rusqlite::Result<(String, Vec<u8>)> {
            Ok((row.get(0)?, row.get(1)?))
        };

        let base = "SELECT id, embedding FROM reflections WHERE embedding IS NOT NULL AND deleted_at IS NULL";
        let sql = match repo {
            Some(_) => format!("{base} AND repo = ?1"),
            None => base.to_owned(),
        };

        let mut stmt = self.conn.prepare(&sql)?;
        let rows = match repo {
            Some(r) => stmt.query_map([r], map_row)?,
            None => stmt.query_map([], map_row)?,
        };
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(LegionError::Database)
    }

    /// Get all reflection IDs that are missing embeddings.
    pub fn get_ids_without_embeddings(&self) -> Result<Vec<(String, String)>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, text FROM reflections WHERE embedding IS NULL AND deleted_at IS NULL ORDER BY created_at DESC",
        )?;
        let rows = stmt.query_map([], |row| {
            let id: String = row.get(0)?;
            let text: String = row.get(1)?;
            Ok((id, text))
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(LegionError::Database)
    }

    /// Retrieve the most recent reflections with embeddings for a repo.
    ///
    /// Returns (id, embedding_blob, text, created_at) tuples, ordered newest
    /// first, for near-duplicate detection on `legion reflect`. Only rows that
    /// have a non-NULL embedding are returned, so reflections that predate the
    /// embed backfill are naturally skipped.
    pub fn get_recent_reflections_with_embeddings(
        &self,
        repo: &str,
        limit: usize,
    ) -> Result<Vec<ReflectionWithEmbedding>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, embedding, text, created_at FROM reflections \
             WHERE repo = ?1 AND embedding IS NOT NULL AND deleted_at IS NULL \
             ORDER BY created_at DESC LIMIT ?2",
        )?;
        let rows = stmt.query_map(rusqlite::params![repo, limit], |row| {
            let id: String = row.get(0)?;
            let blob: Vec<u8> = row.get(1)?;
            let text: String = row.get(2)?;
            let created_at: String = row.get(3)?;
            Ok((id, blob, text, created_at))
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(LegionError::Database)
    }

    /// Increment a reflection's recall count and update last_recalled_at.
    ///
    /// Used by `legion boost` to mark a reflection as useful after being
    /// recalled and applied. Reflections with higher recall counts are
    /// ranked higher in future searches.
    pub fn boost_reflection(&self, id: &str) -> Result<bool> {
        let now = Utc::now().to_rfc3339();
        let rows = self.conn.execute(
            "UPDATE reflections SET recall_count = recall_count + 1, last_recalled_at = ?1, updated_at = ?1 WHERE id = ?2 AND deleted_at IS NULL",
            (&now, id),
        )?;
        Ok(rows > 0)
    }

    /// Retrieve a learning chain starting from the given reflection ID.
    ///
    /// Walks the parent_id links backward to find the chain root, then
    /// walks forward to collect all reflections in chronological order.
    /// Returns an empty vec if the ID does not exist.
    pub fn get_chain(&self, id: &str) -> Result<Vec<Reflection>> {
        // Walk backward to find the root
        let mut root_id = id.to_string();
        loop {
            let r = self.get_reflection_by_id(&root_id)?;
            match r {
                Some(ref reflection) => match &reflection.parent_id {
                    Some(pid) => root_id = pid.clone(),
                    None => break,
                },
                None => break,
            }
        }

        // Walk forward from root collecting children
        let mut chain = Vec::new();
        let mut current_id = Some(root_id);

        while let Some(cid) = current_id {
            match self.get_reflection_by_id(&cid)? {
                Some(r) => {
                    let next = self.find_child(&r.id)?;
                    chain.push(r);
                    current_id = next;
                }
                None => break,
            }
        }

        Ok(chain)
    }

    /// True if this reflection (live, not soft-deleted) participates in a
    /// chain -- either it has a parent or at least one live child. Used by
    /// `whoami` to decide whether to surface a `legion chain --id <id>`
    /// pointer without forcing callers to walk the full chain.
    pub fn is_in_chain(&self, id: &str) -> Result<bool> {
        let row: Option<i64> = self
            .conn
            .query_row(
                "SELECT 1 FROM reflections r \
                 WHERE r.id = ?1 AND r.deleted_at IS NULL \
                   AND (r.parent_id IS NOT NULL \
                        OR EXISTS (SELECT 1 FROM reflections c \
                                   WHERE c.parent_id = r.id AND c.deleted_at IS NULL))",
                [id],
                |row| row.get(0),
            )
            .optional()?;
        Ok(row.is_some())
    }

    /// Find the child reflection that follows the given parent ID.
    fn find_child(&self, parent_id: &str) -> Result<Option<String>> {
        let mut stmt = self.conn.prepare(
            "SELECT id FROM reflections WHERE parent_id = ?1 AND deleted_at IS NULL LIMIT 1",
        )?;
        let mut rows = stmt.query_map([parent_id], |row| row.get::<_, String>(0))?;
        match rows.next() {
            Some(row) => Ok(Some(row?)),
            None => Ok(None),
        }
    }

    /// Retrieve a single reflection by its ID.
    ///
    /// Returns `None` if no reflection exists with the given ID or if soft-deleted.
    pub fn get_reflection_by_id(&self, id: &str) -> Result<Option<Reflection>> {
        let sql = format!(
            "SELECT {REFLECTION_COLUMNS} FROM reflections WHERE id = ?1 AND deleted_at IS NULL"
        );
        let mut stmt = self.conn.prepare(&sql)?;

        let mut rows = stmt.query_map([id], map_reflection_row)?;

        match rows.next() {
            Some(row) => Ok(Some(row?)),
            None => Ok(None),
        }
    }

    /// Read a reflection by id, applying an archive-mode filter (#457).
    /// Hot: returns None if archived_at IS NOT NULL.
    /// Cold: returns None if archived_at IS NULL.
    /// Both: same behavior as `get_reflection_by_id` (no archive filter).
    ///
    /// The deleted_at filter applies in all modes (soft-deleted rows are
    /// always hidden).
    pub fn get_reflection_by_id_in_mode(
        &self,
        id: &str,
        mode: crate::recall::ArchiveMode,
    ) -> Result<Option<Reflection>> {
        let where_clause = match mode {
            crate::recall::ArchiveMode::Hot => "AND archived_at IS NULL",
            crate::recall::ArchiveMode::Cold => "AND archived_at IS NOT NULL",
            crate::recall::ArchiveMode::Both => "",
        };
        let sql = format!(
            "SELECT {REFLECTION_COLUMNS} \
             FROM reflections WHERE id = ?1 AND deleted_at IS NULL {where_clause}"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let mut rows = stmt.query_map([id], map_reflection_row)?;
        match rows.next() {
            Some(row) => Ok(Some(row?)),
            None => Ok(None),
        }
    }

    /// Permanently remove a reflection from the database by id.
    ///
    /// Returns the deleted reflection so the caller can confirm what was
    /// removed (repo, audience, first 80 chars of text, etc). Returns
    /// `LegionError::ReflectionNotFound` if the id does not match any
    /// row.
    ///
    /// This is a HARD delete from SQLite only. Callers must also call
    /// `SearchIndex::delete(id)` to remove the matching document from
    /// the tantivy index, or subsequent BM25 queries will still return
    /// the deleted reflection as a "ghost" until the next reindex.
    ///
    /// Destructive. No soft-delete, no undo. Used by `legion forget` to
    /// retire stale workaround reflections, bad reflections, or personal
    /// data that should not persist in the corpus.
    pub fn delete_reflection(&self, id: &str) -> Result<Reflection> {
        // Fetch first so we can return it and so a missing id produces a
        // clear error rather than a silent zero-row delete.
        let reflection = self
            .get_reflection_by_id(id)?
            .ok_or_else(|| LegionError::ReflectionNotFound(id.to_string()))?;

        let rows = self.conn.execute(
            "DELETE FROM reflections WHERE id = ?1",
            rusqlite::params![id],
        )?;
        if rows == 0 {
            // Race: reflection existed at the fetch above but was
            // deleted before our delete ran. Surface as NotFound rather
            // than success.
            return Err(LegionError::ReflectionNotFound(id.to_string()));
        }
        Ok(reflection)
    }

    /// Soft-delete a reflection by setting its deleted_at timestamp.
    ///
    /// Unlike `delete_reflection` (hard delete), this preserves the row for
    /// multi-node sync tombstone propagation. The row becomes invisible to
    /// normal queries but can still be synced to other nodes.
    #[allow(dead_code)] // Used by sync module in #248
    pub fn soft_delete_reflection(&self, id: &str) -> Result<bool> {
        let now = Utc::now().to_rfc3339();
        let rows = self.conn.execute(
            "UPDATE reflections SET deleted_at = ?1, updated_at = ?1 WHERE id = ?2 AND deleted_at IS NULL",
            rusqlite::params![now, id],
        )?;
        Ok(rows > 0)
    }

    /// Retrieve reflections by a list of IDs. Returns them in the order found
    /// (not necessarily the input order). Missing IDs are silently skipped.
    pub fn get_reflections_by_ids(&self, ids: &[&str]) -> Result<Vec<Reflection>> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let placeholders: Vec<&str> = ids.iter().map(|_| "?").collect();
        let sql = format!(
            "SELECT {REFLECTION_COLUMNS} FROM reflections WHERE id IN ({}) AND deleted_at IS NULL",
            placeholders.join(", ")
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let params: Vec<&dyn rusqlite::types::ToSql> = ids
            .iter()
            .map(|id| id as &dyn rusqlite::types::ToSql)
            .collect();
        let rows = stmt.query_map(params.as_slice(), map_reflection_row)?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(LegionError::Database)
    }

    /// Retrieve all reflections for a repository, ordered newest first.
    #[cfg(test)]
    pub fn get_reflections_by_repo(&self, repo: &str) -> Result<Vec<Reflection>> {
        let sql = format!(
            "SELECT {REFLECTION_COLUMNS} FROM reflections WHERE repo = ?1 AND deleted_at IS NULL ORDER BY created_at DESC"
        );
        let mut stmt = self.conn.prepare(&sql)?;

        let rows = stmt.query_map([repo], map_reflection_row)?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(LegionError::Database)
    }

    /// Retrieve the most recent reflections for a repository, limited by SQL.
    ///
    /// More efficient than `get_reflections_by_repo` when only a small
    /// number of results are needed, since the database handles the LIMIT.
    pub fn get_latest_self_reflections(&self, repo: &str, limit: usize) -> Result<Vec<Reflection>> {
        let sql = format!(
            "SELECT {REFLECTION_COLUMNS} FROM reflections WHERE repo = ?1 AND audience = 'self' AND deleted_at IS NULL ORDER BY created_at DESC LIMIT ?2"
        );
        let mut stmt = self.conn.prepare(&sql)?;

        let rows = stmt.query_map(rusqlite::params![repo, limit], map_reflection_row)?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(LegionError::Database)
    }

    /// Retrieve latest reflections matching a specific domain for a repository.
    ///
    /// Bypasses search entirely -- pure SQL lookup by domain. Used for
    /// reserved domains like `identity` and `snooze` that get injected
    /// on every session start without needing a search query.
    pub fn get_reflections_by_domain(
        &self,
        repo: &str,
        domain: &str,
        limit: usize,
    ) -> Result<Vec<Reflection>> {
        let sql = format!(
            "SELECT {REFLECTION_COLUMNS} \
             FROM reflections WHERE repo = ?1 AND domain = ?2 AND deleted_at IS NULL ORDER BY created_at DESC LIMIT ?3"
        );
        let mut stmt = self.conn.prepare(&sql)?;

        let rows = stmt.query_map(rusqlite::params![repo, domain, limit], map_reflection_row)?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(LegionError::Database)
    }

    /// Retrieve identity reflections that are chain roots or orphans for
    /// `whoami`. Excludes reflections with a `parent_id` (chain children),
    /// because their content is reachable via `legion chain --id <root>`
    /// and including them would bloat the whoami banner past the inline
    /// context budget. Ordered newest first.
    /// Fetch the root reflections (no parent) for a repo in a given domain,
    /// newest first. Backs the SessionStart boot banners: `whoami`
    /// (domain=identity, who I am) and `whatami` (domain=workflow, how I operate).
    pub fn get_domain_roots(
        &self,
        repo: &str,
        domain: &str,
        limit: usize,
    ) -> Result<Vec<Reflection>> {
        let sql = format!(
            "SELECT {REFLECTION_COLUMNS} \
             FROM reflections WHERE repo = ?1 AND domain = ?2 AND parent_id IS NULL AND deleted_at IS NULL ORDER BY created_at DESC LIMIT ?3"
        );
        let mut stmt = self.conn.prepare(&sql)?;

        let rows = stmt.query_map(rusqlite::params![repo, domain, limit], map_reflection_row)?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(LegionError::Database)
    }

    /// Identity roots (domain=identity). Thin wrapper over [`get_domain_roots`].
    pub fn get_identity_roots(&self, repo: &str, limit: usize) -> Result<Vec<Reflection>> {
        self.get_domain_roots(repo, "identity", limit)
    }

    /// Retrieve all reflections for reindexing.
    ///
    /// Returns every reflection in the database regardless of audience or
    /// repo. Used by the `reindex` command to rebuild the search index
    /// from the database (the source of truth).
    pub fn get_all_for_reindex(&self) -> Result<Vec<Reflection>> {
        let sql = format!("SELECT {REFLECTION_COLUMNS} FROM reflections WHERE deleted_at IS NULL");
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map([], map_reflection_row)?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(LegionError::Database)
    }

    /// Get the most recent created_at timestamp from reflections.
    pub fn get_max_created_at(&self) -> Result<Option<String>> {
        let mut stmt = self
            .conn
            .prepare("SELECT MAX(created_at) FROM reflections WHERE deleted_at IS NULL")?;
        let result: Option<String> = stmt
            .query_row([], |row| row.get(0))
            .map_err(LegionError::Database)?;
        Ok(result)
    }

    /// Get recently extended learning chains.
    ///
    /// Returns reflections that have a parent_id and were created within
    /// the last N hours, indicating a chain was recently extended.
    pub fn get_recent_chain_extensions(&self, hours: i64) -> Result<Vec<Reflection>> {
        let cutoff = (Utc::now() - chrono::Duration::hours(hours)).to_rfc3339();
        let sql = format!(
            "SELECT {REFLECTION_COLUMNS} \
             FROM reflections WHERE parent_id IS NOT NULL AND deleted_at IS NULL AND created_at > ?1 ORDER BY created_at DESC"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map([&cutoff], map_reflection_row)?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(LegionError::Database)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::db::testutil::test_db;

    /// Insert an orphan `domain='identity'` row via raw SQL, bypassing the
    /// #785 guard entirely. Simulates the legacy/pre-guard state (or a
    /// synced-in row from `apply_reflection_delta`, which also bypasses the
    /// guard) that `get_identity_roots` and `swap_identity_root` must both
    /// still handle correctly even though `insert_reflection_with_meta`
    /// itself now refuses to create a second one. Returns the new row's id.
    fn insert_raw_orphan_identity(db: &crate::db::Database, repo: &str, text: &str) -> String {
        let id = Uuid::now_v7().to_string();
        let now = Utc::now().to_rfc3339();
        db.conn
            .execute(
                "INSERT INTO reflections (id, repo, text, created_at, domain) \
                 VALUES (?1, ?2, ?3, ?4, 'identity')",
                rusqlite::params![&id, repo, text, &now],
            )
            .unwrap();
        id
    }

    #[test]
    fn insert_and_retrieve_reflection() {
        let db = test_db();
        let r = db
            .insert_reflection("kelex", "mapping rules are fragile", "self")
            .unwrap();
        assert_eq!(r.repo, "kelex");
        assert_eq!(r.text, "mapping rules are fragile");
        assert!(!r.id.is_empty());

        let all = db.get_reflections_by_repo("kelex").unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].id, r.id);
    }

    #[test]
    fn get_reflections_by_ids_round_trip() {
        let db = test_db();
        let root = db
            .insert_reflection("kelex", "root reflection", "self")
            .unwrap();
        let meta = ReflectionMeta {
            domain: Some("workflow".to_string()),
            tags: Some("alpha,beta".to_string()),
            parent_id: Some(root.id.clone()),
        };
        let child = db
            .insert_reflection_with_meta("kelex", "child reflection", "self", &meta)
            .unwrap();

        let ids = [root.id.as_str(), child.id.as_str(), "missing-id"];
        let found = db.get_reflections_by_ids(&ids).unwrap();
        // Missing IDs are silently skipped.
        assert_eq!(found.len(), 2);

        let got_root = found.iter().find(|r| r.id == root.id).unwrap();
        assert_eq!(got_root.text, "root reflection");
        // Regression for #606: the SELECT omitted updated_at, shifting every
        // column from index 4 on (audience loaded into updated_at, and the
        // parent_id read indexed past the row -- InvalidColumnIndex).
        assert_eq!(
            got_root.updated_at.as_deref(),
            Some(root.created_at.as_str())
        );
        assert_eq!(got_root.audience, "self");
        assert_eq!(got_root.parent_id, None);

        let got_child = found.iter().find(|r| r.id == child.id).unwrap();
        assert_eq!(
            got_child.updated_at.as_deref(),
            Some(child.created_at.as_str())
        );
        assert_eq!(got_child.domain.as_deref(), Some("workflow"));
        assert_eq!(got_child.tags.as_deref(), Some("alpha,beta"));
        assert_eq!(got_child.parent_id.as_deref(), Some(root.id.as_str()));
    }

    #[test]
    fn get_reflections_by_ids_empty_input() {
        let db = test_db();
        assert!(db.get_reflections_by_ids(&[]).unwrap().is_empty());
    }

    #[test]
    fn updated_at_set_on_insert() {
        let db = test_db();
        let r = db
            .insert_reflection("test", "test reflection", "self")
            .unwrap();
        // updated_at should be set to created_at on insert
        assert!(r.updated_at.is_some());
        assert_eq!(r.updated_at.as_ref().unwrap(), &r.created_at);
    }

    #[test]
    fn updated_at_refreshed_on_boost() {
        let db = test_db();
        let r = db
            .insert_reflection("test", "test reflection", "self")
            .unwrap();
        let original_updated_at = r.updated_at.clone();

        // Small delay to ensure timestamp differs
        std::thread::sleep(std::time::Duration::from_millis(10));

        db.boost_reflection(&r.id).unwrap();

        let boosted = db.get_reflection_by_id(&r.id).unwrap().unwrap();
        assert!(boosted.updated_at.is_some());
        // updated_at should be later than the original
        assert!(boosted.updated_at.unwrap() > original_updated_at.unwrap());
    }

    #[test]
    fn reflections_scoped_to_repo() {
        let db = test_db();
        db.insert_reflection("kelex", "reflection 1", "self")
            .unwrap();
        db.insert_reflection("rafters", "reflection 2", "self")
            .unwrap();

        let kelex = db.get_reflections_by_repo("kelex").unwrap();
        assert_eq!(kelex.len(), 1);
        assert_eq!(kelex[0].text, "reflection 1");
    }

    #[test]
    fn ids_are_uuidv7() {
        let db = test_db();
        let r = db.insert_reflection("test", "text", "self").unwrap();
        assert_eq!(r.id.len(), 36);
        // UUIDv7 has version nibble '7' at position 14
        assert_eq!(&r.id[14..15], "7");
    }

    #[test]
    fn created_at_is_iso8601() {
        let db = test_db();
        let r = db.insert_reflection("test", "text", "self").unwrap();
        // ISO 8601 strings contain 'T' separator and '+' or end with 'Z'
        assert!(r.created_at.contains('T'));
    }

    #[test]
    fn empty_repo_returns_empty_vec() {
        let db = test_db();
        let results = db.get_reflections_by_repo("nonexistent").unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn insert_reflection_with_audience_self() {
        let db = test_db();
        let r = db.insert_reflection("kelex", "test", "self").unwrap();
        assert_eq!(r.audience, "self");
    }

    #[test]
    fn insert_reflection_with_audience_team() {
        let db = test_db();
        let r = db
            .insert_reflection("rafters", "night shift musings", "team")
            .unwrap();
        assert_eq!(r.audience, "team");
    }

    #[test]
    fn get_all_for_reindex_returns_all_reflections() {
        let db = test_db();
        db.insert_reflection("kelex", "one", "self").unwrap();
        db.insert_reflection("rafters", "two", "team").unwrap();
        db.insert_reflection("platform", "three", "self").unwrap();

        let all = db.get_all_for_reindex().unwrap();
        assert_eq!(all.len(), 3);

        let repos: Vec<&str> = all.iter().map(|r| r.repo.as_str()).collect();
        assert!(repos.contains(&"kelex"));
        assert!(repos.contains(&"rafters"));
        assert!(repos.contains(&"platform"));
    }

    #[test]
    fn get_all_for_reindex_empty_db() {
        let db = test_db();
        let all = db.get_all_for_reindex().unwrap();
        assert!(all.is_empty());
    }

    #[test]
    fn existing_reflections_default_to_self() {
        let db = test_db();
        let r = db
            .insert_reflection("test", "old reflection", "self")
            .unwrap();
        assert_eq!(r.audience, "self");
        let posts = db.get_board_posts().unwrap();
        assert!(posts.is_empty());
    }

    #[test]
    fn insert_with_meta_stores_domain_and_tags() {
        let db = test_db();
        let meta = ReflectionMeta {
            domain: Some("color-tokens".into()),
            tags: Some("semantic-tokens,consumer".into()),
            parent_id: None,
        };
        let r = db
            .insert_reflection_with_meta("kelex", "oklch insight", "self", &meta)
            .unwrap();
        assert_eq!(r.domain.as_deref(), Some("color-tokens"));
        assert_eq!(r.tags.as_deref(), Some("semantic-tokens,consumer"));
        assert!(r.parent_id.is_none());

        let fetched = db.get_reflection_by_id(&r.id).unwrap().unwrap();
        assert_eq!(fetched.domain.as_deref(), Some("color-tokens"));
        assert_eq!(fetched.tags.as_deref(), Some("semantic-tokens,consumer"));
    }

    #[test]
    fn insert_with_meta_stores_parent_id() {
        let db = test_db();
        let parent = db.insert_reflection("kelex", "first", "self").unwrap();
        let meta = ReflectionMeta {
            domain: None,
            tags: None,
            parent_id: Some(parent.id.clone()),
        };
        let child = db
            .insert_reflection_with_meta("kelex", "follows up", "self", &meta)
            .unwrap();
        assert_eq!(child.parent_id.as_deref(), Some(parent.id.as_str()));
    }

    #[test]
    fn boost_increments_recall_count() {
        let db = test_db();
        let r = db
            .insert_reflection("kelex", "useful insight", "self")
            .unwrap();
        assert_eq!(r.recall_count, 0);
        assert!(r.last_recalled_at.is_none());

        let found = db.boost_reflection(&r.id).unwrap();
        assert!(found);

        let boosted = db.get_reflection_by_id(&r.id).unwrap().unwrap();
        assert_eq!(boosted.recall_count, 1);
        assert!(boosted.last_recalled_at.is_some());

        db.boost_reflection(&r.id).unwrap();
        let double = db.get_reflection_by_id(&r.id).unwrap().unwrap();
        assert_eq!(double.recall_count, 2);
    }

    #[test]
    fn boost_nonexistent_returns_false() {
        let db = test_db();
        let found = db.boost_reflection("nonexistent-id").unwrap();
        assert!(!found);
    }

    #[test]
    fn get_chain_single_node() {
        let db = test_db();
        let r = db.insert_reflection("kelex", "standalone", "self").unwrap();
        let chain = db.get_chain(&r.id).unwrap();
        assert_eq!(chain.len(), 1);
        assert_eq!(chain[0].id, r.id);
    }

    #[test]
    fn get_chain_three_links() {
        let db = test_db();
        let first = db
            .insert_reflection("kelex", "root insight", "self")
            .unwrap();
        let second = db
            .insert_reflection_with_meta(
                "kelex",
                "builds on root",
                "self",
                &ReflectionMeta {
                    parent_id: Some(first.id.clone()),
                    ..Default::default()
                },
            )
            .unwrap();
        let third = db
            .insert_reflection_with_meta(
                "kelex",
                "final refinement",
                "self",
                &ReflectionMeta {
                    parent_id: Some(second.id.clone()),
                    ..Default::default()
                },
            )
            .unwrap();

        // Querying from any node should return the full chain in order
        let chain = db.get_chain(&third.id).unwrap();
        assert_eq!(chain.len(), 3);
        assert_eq!(chain[0].id, first.id);
        assert_eq!(chain[1].id, second.id);
        assert_eq!(chain[2].id, third.id);

        let from_middle = db.get_chain(&second.id).unwrap();
        assert_eq!(from_middle.len(), 3);
        assert_eq!(from_middle[0].id, first.id);
    }

    #[test]
    fn get_identity_roots_excludes_chain_children() {
        let db = test_db();
        let root = db
            .insert_reflection_with_meta(
                "legion",
                "root identity",
                "self",
                &ReflectionMeta {
                    domain: Some("identity".into()),
                    ..Default::default()
                },
            )
            .unwrap();
        let _child = db
            .insert_reflection_with_meta(
                "legion",
                "chain child identity",
                "self",
                &ReflectionMeta {
                    domain: Some("identity".into()),
                    parent_id: Some(root.id.clone()),
                    ..Default::default()
                },
            )
            .unwrap();

        // A second orphan root can no longer be created through
        // `insert_reflection_with_meta` -- the #785 guard refuses it. This
        // test still needs to prove `get_identity_roots` surfaces
        // pre-existing/legacy orphan rows (predating the guard, or arriving
        // via a sync race) rather than silently hiding them, so the orphan
        // fixture goes in via a raw INSERT that deliberately bypasses the
        // guard, exactly as a legacy row or a synced-in row would look.
        let orphan_id = insert_raw_orphan_identity(&db, "legion", "orphan identity");

        let roots = db.get_identity_roots("legion", 10).unwrap();
        let ids: Vec<&str> = roots.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(roots.len(), 2);
        assert!(ids.contains(&root.id.as_str()));
        assert!(ids.contains(&orphan_id.as_str()));
    }

    #[test]
    fn insert_reflection_with_meta_rejects_second_identity_root() {
        let db = test_db();
        let first = db
            .insert_reflection_with_meta(
                "legion",
                "first identity",
                "self",
                &ReflectionMeta {
                    domain: Some("identity".into()),
                    ..Default::default()
                },
            )
            .unwrap();

        let result = db.insert_reflection_with_meta(
            "legion",
            "second identity",
            "self",
            &ReflectionMeta {
                domain: Some("identity".into()),
                ..Default::default()
            },
        );

        match result {
            Err(LegionError::IdentityRootExists { repo, existing_id }) => {
                assert_eq!(repo, "legion");
                assert_eq!(existing_id, first.id);
            }
            other => panic!("expected IdentityRootExists, got {other:?}"),
        }
    }

    #[test]
    fn insert_reflection_with_meta_allows_identity_child_when_root_exists() {
        let db = test_db();
        let root = db
            .insert_reflection_with_meta(
                "legion",
                "root identity",
                "self",
                &ReflectionMeta {
                    domain: Some("identity".into()),
                    ..Default::default()
                },
            )
            .unwrap();

        let child = db
            .insert_reflection_with_meta(
                "legion",
                "chained identity beat",
                "self",
                &ReflectionMeta {
                    domain: Some("identity".into()),
                    parent_id: Some(root.id.clone()),
                    ..Default::default()
                },
            )
            .unwrap();

        assert_eq!(child.parent_id.as_deref(), Some(root.id.as_str()));
    }

    #[test]
    fn insert_reflection_with_meta_allows_first_identity_root_when_repo_empty() {
        let db = test_db();
        let root = db
            .insert_reflection_with_meta(
                "fresh-repo",
                "bootstrap identity",
                "self",
                &ReflectionMeta {
                    domain: Some("identity".into()),
                    ..Default::default()
                },
            )
            .unwrap();
        assert!(root.parent_id.is_none());
    }

    #[test]
    fn insert_reflection_with_meta_ignores_archived_root_for_guard() {
        // The guard's own liveness read filters archived_at IS NULL (#785
        // build note), independently of get_identity_roots (which does
        // not filter archived_at yet -- #782). An archived root must not
        // block a fresh bootstrap insert.
        //
        // Documenting the consequence, not just the guard's own behavior:
        // once this insert succeeds, get_identity_roots("legion", ..) DOES
        // return both rows (asserted below) -- the pre-#782 banner reader
        // would show two roots again if an identity reflection were ever
        // archived. No current write path can do that: `legion reflect`
        // always stores identity reflections with audience="self"
        // (hardcoded, no --audience flag exists), and the only production
        // writer of `archived_at` on the reflections table
        // (`Database::archive_read_posts`, src/db/board.rs) is scoped to
        // `audience = 'team'`. This test's archive step is a raw UPDATE
        // simulating a future/hypothetical writer, not a reachable path
        // today.
        let db = test_db();
        let root = db
            .insert_reflection_with_meta(
                "legion",
                "old archived identity",
                "self",
                &ReflectionMeta {
                    domain: Some("identity".into()),
                    ..Default::default()
                },
            )
            .unwrap();
        let now = Utc::now().to_rfc3339();
        db.conn
            .execute(
                "UPDATE reflections SET archived_at = ?1 WHERE id = ?2",
                rusqlite::params![&now, &root.id],
            )
            .unwrap();

        let fresh = db
            .insert_reflection_with_meta(
                "legion",
                "new identity after archive",
                "self",
                &ReflectionMeta {
                    domain: Some("identity".into()),
                    ..Default::default()
                },
            )
            .unwrap();
        assert!(fresh.parent_id.is_none());
        assert_ne!(fresh.id, root.id);

        // The archived root and the fresh root now BOTH satisfy
        // get_identity_roots's current (pre-#782) predicate -- this is
        // the documented, accepted asymmetry, made visible here rather
        // than left implicit.
        let roots = db.get_identity_roots("legion", 10).unwrap();
        assert_eq!(
            roots.len(),
            2,
            "pre-#782, get_identity_roots does not filter archived_at, so \
             an archived root and a fresh root both surface -- tracked, not \
             regressed, by this guard"
        );
    }

    #[test]
    fn swap_identity_root_replaces_existing_root() {
        let db = test_db();
        let old_root = db
            .insert_reflection_with_meta(
                "legion",
                "old identity",
                "self",
                &ReflectionMeta {
                    domain: Some("identity".into()),
                    ..Default::default()
                },
            )
            .unwrap();

        let result = db
            .swap_identity_root("legion", "new identity", &[], "self")
            .unwrap();

        assert_eq!(result.deleted_ids, vec![old_root.id.clone()]);
        assert_eq!(result.root.text, "new identity");
        assert!(result.root.parent_id.is_none());
        assert!(result.children.is_empty());

        // Old root is gone, new root is the only live root.
        assert!(db.get_reflection_by_id(&old_root.id).unwrap().is_none());
        let roots = db.get_identity_roots("legion", 10).unwrap();
        assert_eq!(roots.len(), 1);
        assert_eq!(roots[0].id, result.root.id);
    }

    #[test]
    fn swap_identity_root_does_not_cascade_to_old_chain_children() {
        // Locks in the documented non-cascade decision: swap_identity_root
        // deletes only the root row, matching legion forget's existing
        // single-row delete_reflection (no cascade). The old root's
        // --follows child survives as a live row with a now-dangling
        // parent_id -- exactly what forget already leaves behind today,
        // and what a future cascade (if #784 ever needs one) would change.
        let db = test_db();
        let old_root = db
            .insert_reflection_with_meta(
                "legion",
                "old identity",
                "self",
                &ReflectionMeta {
                    domain: Some("identity".into()),
                    ..Default::default()
                },
            )
            .unwrap();
        let old_child = db
            .insert_reflection_with_meta(
                "legion",
                "old supporting beat",
                "self",
                &ReflectionMeta {
                    domain: Some("identity".into()),
                    parent_id: Some(old_root.id.clone()),
                    ..Default::default()
                },
            )
            .unwrap();

        let result = db
            .swap_identity_root("legion", "new identity", &[], "self")
            .unwrap();

        assert_eq!(result.deleted_ids, vec![old_root.id.clone()]);
        assert!(
            db.get_reflection_by_id(&old_root.id).unwrap().is_none(),
            "root must be deleted"
        );

        // The old child was NOT deleted -- it survives, dangling.
        let surviving_child = db
            .get_reflection_by_id(&old_child.id)
            .unwrap()
            .expect("old chain child must survive the swap (non-cascading delete)");
        assert_eq!(
            surviving_child.parent_id.as_deref(),
            Some(old_root.id.as_str()),
            "surviving child's parent_id still points at the now-deleted old root"
        );

        // The new root is the only row get_identity_roots surfaces --
        // the dangling child has parent_id set, so it never counted as a
        // root and does not resurrect the old identity in the banner.
        let roots = db.get_identity_roots("legion", 10).unwrap();
        assert_eq!(roots.len(), 1);
        assert_eq!(roots[0].id, result.root.id);
    }

    #[test]
    fn swap_identity_root_inserts_chained_children() {
        let db = test_db();
        db.insert_reflection_with_meta(
            "legion",
            "old identity",
            "self",
            &ReflectionMeta {
                domain: Some("identity".into()),
                ..Default::default()
            },
        )
        .unwrap();

        let result = db
            .swap_identity_root(
                "legion",
                "new root",
                &["supporting beat one", "supporting beat two"],
                "self",
            )
            .unwrap();

        assert_eq!(result.children.len(), 2);
        assert_eq!(
            result.children[0].parent_id.as_deref(),
            Some(result.root.id.as_str())
        );
        assert_eq!(
            result.children[1].parent_id.as_deref(),
            Some(result.children[0].id.as_str())
        );

        // get_chain from the root walks forward through both children.
        let chain = db.get_chain(&result.root.id).unwrap();
        assert_eq!(chain.len(), 3);
        assert_eq!(chain[0].id, result.root.id);
        assert_eq!(chain[1].id, result.children[0].id);
        assert_eq!(chain[2].id, result.children[1].id);
    }

    #[test]
    fn swap_identity_root_cleans_up_multiple_pre_existing_orphans() {
        // Simulates the pre-#785 leaked state: two orphan identity roots
        // already exist (inserted via raw SQL, bypassing the guard, the
        // same way the guard test above builds its legacy fixture). Swap
        // must clear all of them, not just one.
        let db = test_db();
        let old_ids: Vec<String> = ["orphan one", "orphan two"]
            .into_iter()
            .map(|text| insert_raw_orphan_identity(&db, "legion", text))
            .collect();

        let result = db
            .swap_identity_root("legion", "clean new identity", &[], "self")
            .unwrap();

        let mut deleted = result.deleted_ids.clone();
        deleted.sort();
        let mut expected = old_ids.clone();
        expected.sort();
        assert_eq!(deleted, expected);

        let roots = db.get_identity_roots("legion", 10).unwrap();
        assert_eq!(roots.len(), 1);
        assert_eq!(roots[0].id, result.root.id);
    }

    #[test]
    fn swap_identity_root_bootstraps_when_no_prior_root_exists() {
        let db = test_db();
        let result = db
            .swap_identity_root("fresh-repo", "first identity via swap", &[], "self")
            .unwrap();
        assert!(result.deleted_ids.is_empty());
        assert_eq!(result.root.text, "first identity via swap");
    }

    #[test]
    fn get_identity_roots_filters_by_repo_and_domain() {
        let db = test_db();
        let _other_domain = db
            .insert_reflection_with_meta(
                "legion",
                "not identity",
                "self",
                &ReflectionMeta {
                    domain: Some("architecture".into()),
                    ..Default::default()
                },
            )
            .unwrap();
        let _other_repo = db
            .insert_reflection_with_meta(
                "kelex",
                "wrong repo",
                "self",
                &ReflectionMeta {
                    domain: Some("identity".into()),
                    ..Default::default()
                },
            )
            .unwrap();
        let target = db
            .insert_reflection_with_meta(
                "legion",
                "right one",
                "self",
                &ReflectionMeta {
                    domain: Some("identity".into()),
                    ..Default::default()
                },
            )
            .unwrap();

        let roots = db.get_identity_roots("legion", 10).unwrap();
        assert_eq!(roots.len(), 1);
        assert_eq!(roots[0].id, target.id);
    }

    #[test]
    fn get_chain_nonexistent_returns_empty() {
        let db = test_db();
        let chain = db.get_chain("nonexistent").unwrap();
        assert!(chain.is_empty());
    }

    #[test]
    fn get_reflection_by_id_found() {
        let db = test_db();
        let r = db.insert_reflection("kelex", "findable", "self").unwrap();
        let found = db.get_reflection_by_id(&r.id).unwrap();
        assert!(found.is_some());
        assert_eq!(found.unwrap().text, "findable");
    }

    #[test]
    fn get_reflection_by_id_not_found() {
        let db = test_db();
        let found = db.get_reflection_by_id("no-such-id").unwrap();
        assert!(found.is_none());
    }

    #[test]
    fn get_recent_reflections_with_embeddings_returns_only_embedded() {
        let db = test_db();
        // Insert two reflections; only give one an embedding.
        let r1 = db
            .insert_reflection("kelex", "has embedding", "self")
            .unwrap();
        let _r2 = db
            .insert_reflection("kelex", "no embedding", "self")
            .unwrap();

        let blob = vec![0u8; 256 * 4]; // 256 f32 zeros
        db.store_embedding(&r1.id, &blob).unwrap();

        let results = db
            .get_recent_reflections_with_embeddings("kelex", 10)
            .unwrap();
        assert_eq!(
            results.len(),
            1,
            "only the embedded reflection should appear"
        );
        assert_eq!(results[0].0, r1.id);
    }

    #[test]
    fn get_recent_reflections_with_embeddings_respects_repo_scope() {
        let db = test_db();
        let r_kelex = db.insert_reflection("kelex", "kelex text", "self").unwrap();
        let r_rafters = db
            .insert_reflection("rafters", "rafters text", "self")
            .unwrap();

        let blob = vec![0u8; 256 * 4];
        db.store_embedding(&r_kelex.id, &blob).unwrap();
        db.store_embedding(&r_rafters.id, &blob).unwrap();

        let kelex_results = db
            .get_recent_reflections_with_embeddings("kelex", 10)
            .unwrap();
        assert_eq!(kelex_results.len(), 1);
        assert_eq!(kelex_results[0].0, r_kelex.id);
    }

    #[test]
    fn get_recent_reflections_with_embeddings_respects_limit() {
        let db = test_db();
        let blob = vec![0u8; 256 * 4];

        for i in 0..5 {
            let r = db
                .insert_reflection("legion", &format!("reflection {i}"), "self")
                .unwrap();
            db.store_embedding(&r.id, &blob).unwrap();
        }

        let results = db
            .get_recent_reflections_with_embeddings("legion", 3)
            .unwrap();
        assert_eq!(results.len(), 3);
    }

    #[test]
    fn delete_reflection_removes_row_and_returns_deleted() {
        let db = test_db();

        // Insert a reflection via the real path so the schema columns
        // are all populated exactly as production would.
        let inserted = db
            .insert_reflection("shingle", "stale workaround doctrine", "self")
            .unwrap();

        // Confirm it is visible before the delete.
        assert!(db.get_reflection_by_id(&inserted.id).unwrap().is_some());

        // Delete, confirm the returned row matches what was stored.
        let deleted = db.delete_reflection(&inserted.id).expect("delete");
        assert_eq!(deleted.id, inserted.id);
        assert_eq!(deleted.repo, "shingle");
        assert_eq!(deleted.text, "stale workaround doctrine");

        // Gone from the table.
        assert!(db.get_reflection_by_id(&inserted.id).unwrap().is_none());

        // Second delete on the same id returns ReflectionNotFound,
        // not silent success.
        let result = db.delete_reflection(&inserted.id);
        assert!(
            matches!(result, Err(LegionError::ReflectionNotFound(_))),
            "expected ReflectionNotFound, got {:?}",
            result
        );
    }

    #[test]
    fn soft_delete_reflection_hides_from_queries() {
        let db = test_db();

        // Insert a reflection.
        let r = db
            .insert_reflection("test-repo", "soft delete test reflection", "self")
            .unwrap();
        let id = r.id;
        assert!(db.get_reflection_by_id(&id).unwrap().is_some());

        // Soft delete it.
        let deleted = db.soft_delete_reflection(&id).unwrap();
        assert!(deleted, "soft_delete_reflection should return true");

        // The reflection should now be invisible to normal queries.
        assert!(
            db.get_reflection_by_id(&id).unwrap().is_none(),
            "soft-deleted reflection should not be visible"
        );

        // Soft deleting again returns false (already deleted).
        let deleted_again = db.soft_delete_reflection(&id).unwrap();
        assert!(
            !deleted_again,
            "soft_delete_reflection on already-deleted should return false"
        );
    }
}
