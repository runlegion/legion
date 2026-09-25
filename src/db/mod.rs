//! SQLite persistence layer, split by domain (#609).
//!
//! This module owns infrastructure only: the [`Database`] handle, `open`,
//! `has_column`, the `init_schema` dispatcher, and cross-table admin
//! (`rename_repo`). Every domain file carries its own `impl Database`
//! block plus the DDL and tests for its tables, so the public API is
//! identical to the old single-file `db.rs`.

mod audit;
mod autonomy;
mod board;
pub(crate) mod card_criteria;
pub mod css_symbols;
mod defer;
mod documents;
pub(crate) mod findings;
mod health;
mod heartbeat;
pub mod inventory;
mod kanban;
pub mod module_edges;
pub(crate) mod quality_gates;
mod reflections;
mod schedules;
mod scip;
mod sessions;
mod stats;
mod statusline_samples;
mod sync;
#[cfg(test)]
pub(crate) mod testutil;
mod uncertainty;
mod wake;

pub use audit::AuditInput;
pub use board::{INBOX_CURSOR_SUFFIX, RedeliveryOutcome};
pub use reflections::{Reflection, ReflectionMeta};
pub use schedules::validate_hhmm;

use std::path::Path;
use std::time::{Duration, Instant};

use chrono::Utc;
use rusqlite::{Connection, ErrorCode, TransactionBehavior};

use crate::error::{LegionError, Result};

/// Format an ISO 8601 timestamp to a date-only string (YYYY-MM-DD).
///
/// Falls back to the raw value if parsing fails, which keeps output
/// usable even with unexpected timestamp formats.
pub(crate) fn format_date(iso_timestamp: &str) -> &str {
    match iso_timestamp.split_once('T') {
        Some((date, _)) => date,
        None => iso_timestamp,
    }
}

/// Default busy timeout applied to every connection opened via
/// [`Database::open`], matching the value `sync_actor` has used
/// historically for its own explicit override (src/sync_actor.rs).
///
/// rusqlite's bundled SQLite already applies its own 5s `busy_timeout`
/// default on `Connection::open`, so this isn't closing a zero-timeout
/// gap in practice -- it pins every CLI connection (`open_db` /
/// `open_db_and_index`, src/cli/util.rs) to the *same* explicit value
/// `sync_actor` uses, rather than leaving them dependent on an
/// undocumented library default that could change or drift out of sync
/// with sync_actor's own setting (#721).
const DEFAULT_BUSY_TIMEOUT: Duration = Duration::from_secs(2);

/// Schema stamp written to `PRAGMA user_version` once [`Database::open`]
/// has run the full create-and-migrate chain (#1289). An open that finds
/// this value (or newer) skips the chain and its write lock entirely.
///
/// Bump this whenever a `create_tables` or `migrate` step changes the
/// schema: an already-stamped store only re-runs the chain when its stamp
/// is below this value, so a new step without a bump never reaches it.
/// `schema_fingerprint_is_pinned_to_schema_version` fails on a schema
/// change until this is bumped and the fingerprint re-pinned.
const SCHEMA_VERSION: i32 = 1;

/// Persistent storage for reflections backed by SQLite.
pub struct Database {
    pub(crate) conn: Connection,
}

impl Database {
    /// Open (or create) a SQLite database at the given path.
    ///
    /// Parent directories are created automatically if they do not exist.
    /// WAL mode is enabled for concurrent read performance. A default
    /// busy timeout (see [`DEFAULT_BUSY_TIMEOUT`]) bounds how long a
    /// connection retries against `SQLITE_BUSY` before giving up.
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let mut conn = Connection::open(path)?;
        conn.busy_timeout(DEFAULT_BUSY_TIMEOUT)
            .map_err(LegionError::Database)?;

        Self::enable_wal(&conn)?;
        Self::init_schema(&mut conn)?;

        Ok(Self { conn })
    }

    /// Switch the store to WAL journaling unless it already is.
    ///
    /// SQLite does not run the busy handler for the lock the WAL switch
    /// needs, so a fresh store opened by several connections at once
    /// fails all but one switch with `SQLITE_BUSY` immediately (#1289).
    /// A busy switch is retried until [`DEFAULT_BUSY_TIMEOUT`] runs out,
    /// re-reading the mode first so a switch another opener already made
    /// ends the loop.
    fn enable_wal(conn: &Connection) -> Result<()> {
        let deadline = Instant::now() + DEFAULT_BUSY_TIMEOUT;
        loop {
            let mode: String = conn
                .pragma_query_value(None, "journal_mode", |row| row.get(0))
                .map_err(LegionError::Database)?;
            if mode == "wal" {
                return Ok(());
            }
            match conn.pragma_update(None, "journal_mode", "WAL") {
                Err(e)
                    if e.sqlite_error_code() == Some(ErrorCode::DatabaseBusy)
                        && Instant::now() < deadline =>
                {
                    std::thread::sleep(Duration::from_millis(5));
                }
                result => return result.map_err(LegionError::Database),
            }
        }
    }

    /// Read the store's `PRAGMA user_version` schema stamp.
    fn schema_version(conn: &Connection) -> Result<i32> {
        conn.pragma_query_value(None, "user_version", |row| row.get(0))
            .map_err(LegionError::Database)
    }

    /// Check whether a table has a specific column via PRAGMA table_info.
    fn has_column(conn: &Connection, table: &str, column: &str) -> Result<bool> {
        let mut stmt = conn.prepare(&format!("PRAGMA table_info({})", table))?;
        let names: Vec<String> = stmt
            .query_map([], |row| {
                let name: String = row.get(1)?;
                Ok(name)
            })?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(LegionError::Database)?;
        Ok(names.iter().any(|n| n == column))
    }

    /// Create every table and index, then run the column migrations,
    /// serialized across every connection opening this store (#1289).
    ///
    /// A store stamped with [`SCHEMA_VERSION`] (or newer) returns after
    /// one `user_version` read, taking no lock. Otherwise the whole chain
    /// runs inside one `BEGIN IMMEDIATE` transaction: the write lock is
    /// taken up front (waiting out [`DEFAULT_BUSY_TIMEOUT`] behind another
    /// opener), the stamp is re-read under the lock so an opener that
    /// lost the race to a finished migration does nothing, and the stamp
    /// is written in the same commit as the schema. Two opens of a fresh
    /// store therefore never interleave has_column checks and ALTERs.
    ///
    /// Each domain file owns its DDL: the per-domain `create_tables`
    /// functions run the CREATE TABLE / CREATE INDEX statements for the
    /// base shape, and the `migrate` steps (has_column-guarded ALTERs,
    /// their backfills, and indexes over migrated columns) run after, in
    /// the same relative order they held in the single-file init_schema.
    fn init_schema(conn: &mut Connection) -> Result<()> {
        if Self::schema_version(conn)? >= SCHEMA_VERSION {
            return Ok(());
        }

        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if Self::schema_version(&tx)? >= SCHEMA_VERSION {
            return Ok(());
        }

        reflections::create_tables(&tx)?;
        board::create_tables(&tx)?;
        kanban::create_tables(&tx)?;
        defer::create_tables(&tx)?;
        schedules::create_tables(&tx)?;
        health::create_tables(&tx)?;
        audit::create_tables(&tx)?;
        quality_gates::create_tables(&tx)?;
        findings::create_tables(&tx)?;
        statusline_samples::create_tables(&tx)?;
        wake::create_tables(&tx)?;
        scip::create_tables(&tx)?;
        sessions::create_tables(&tx)?;
        documents::create_tables(&tx)?;
        uncertainty::create_tables(&tx)?;
        autonomy::create_tables(&tx)?;
        heartbeat::create_tables(&tx)?;
        inventory::create_tables(&tx)?;
        module_edges::create_tables(&tx)?;
        css_symbols::create_tables(&tx)?;

        reflections::migrate(&tx)?;
        board::migrate(&tx)?;
        kanban::migrate(&tx)?;
        schedules::migrate(&tx)?;
        wake::migrate(&tx)?;
        quality_gates::migrate(&tx)?;
        documents::migrate(&tx)?;

        tx.pragma_update(None, "user_version", SCHEMA_VERSION)?;
        tx.commit()?;
        Ok(())
    }

    /// Rename a repo across all tables. Returns total rows updated.
    pub fn rename_repo(&self, from: &str, to: &str) -> Result<RenameCounts> {
        // unchecked_transaction because Database uses &self (shared ref),
        // but rusqlite::Connection::transaction() requires &mut self.
        // Safe here: no concurrent access within this function.
        let tx = self.conn.unchecked_transaction()?;
        let now = Utc::now().to_rfc3339();

        let reflections = tx.execute(
            "UPDATE reflections SET repo = ?1, updated_at = ?3 WHERE repo = ?2",
            rusqlite::params![to, from, &now],
        )? as u64;

        let tasks_from = tx.execute(
            "UPDATE tasks SET from_repo = ?1 WHERE from_repo = ?2",
            [to, from],
        )? as u64;

        let tasks_to = tx.execute(
            "UPDATE tasks SET to_repo = ?1 WHERE to_repo = ?2",
            [to, from],
        )? as u64;

        // board_reads, watch_handled, and watch_redelivery (#948) all key on
        // a repo-name column with no room for two rows under the same repo,
        // so each needs the target's existing rows deleted first to avoid a
        // PK collision before the source's rows are renamed in. Every other
        // table above is a bare UPDATE with no such collision risk.
        let board_reads =
            Self::carry_repo_keyed_table(&tx, "board_reads", "reader_repo", to, from)?;
        let watch_handled =
            Self::carry_repo_keyed_table(&tx, "watch_handled", "repo_name", to, from)?;
        let watch_redelivery =
            Self::carry_repo_keyed_table(&tx, "watch_redelivery", "repo_name", to, from)?;

        let schedules = tx.execute(
            "UPDATE schedules SET repo = ?1, updated_at = ?3 WHERE repo = ?2",
            rusqlite::params![to, from, &now],
        )? as u64;

        tx.commit()?;

        Ok(RenameCounts {
            reflections,
            tasks_from,
            tasks_to,
            board_reads,
            watch_handled,
            watch_redelivery,
            schedules,
        })
    }

    /// Delete-then-rename for a `rename_repo` target whose key includes a
    /// repo-name column: deletes the destination repo's existing rows (a PK
    /// collision would otherwise abort the rename), then renames the source
    /// repo's rows in. `table` and `repo_column` are call-site literals
    /// (`board_reads`/`reader_repo`, `watch_handled`/`repo_name`,
    /// `watch_redelivery`/`repo_name`), never caller-supplied input, so the
    /// `format!`-built SQL carries no injection risk.
    fn carry_repo_keyed_table(
        tx: &rusqlite::Transaction,
        table: &str,
        repo_column: &str,
        to: &str,
        from: &str,
    ) -> Result<u64> {
        tx.execute(
            &format!("DELETE FROM {table} WHERE {repo_column} = ?1"),
            [to],
        )?;
        let count = tx.execute(
            &format!("UPDATE {table} SET {repo_column} = ?1 WHERE {repo_column} = ?2"),
            [to, from],
        )?;
        Ok(count as u64)
    }
}

/// Counts of rows updated by a repo rename.
#[derive(Debug)]
pub struct RenameCounts {
    pub reflections: u64,
    pub tasks_from: u64,
    pub tasks_to: u64,
    pub board_reads: u64,
    pub watch_handled: u64,
    pub watch_redelivery: u64,
    pub schedules: u64,
}

impl RenameCounts {
    pub fn total(&self) -> u64 {
        self.reflections
            + self.tasks_from
            + self.tasks_to
            + self.board_reads
            + self.watch_handled
            + self.watch_redelivery
            + self.schedules
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::db::testutil::test_db;

    #[test]
    fn open_creates_database() {
        let dir = tempfile::tempdir().unwrap();
        let _db = Database::open(&dir.path().join("test.db")).unwrap();
        assert!(dir.path().join("test.db").exists());
    }

    #[test]
    fn open_creates_parent_directories() {
        let dir = tempfile::tempdir().unwrap();
        let nested = dir.path().join("a").join("b").join("c").join("test.db");
        let _db = Database::open(&nested).unwrap();
        assert!(nested.exists());
    }

    #[test]
    fn init_schema_migrates_a_v1_database() {
        // A database created at the original v1 shape (base reflections
        // table only) must come up through every column migration when
        // reopened through the split dispatcher, ending insert-ready.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("old.db");
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(
                "CREATE TABLE reflections (
                    id TEXT PRIMARY KEY,
                    repo TEXT NOT NULL,
                    text TEXT NOT NULL,
                    created_at TEXT NOT NULL,
                    embedding BLOB
                );",
            )
            .unwrap();
            conn.execute(
                "INSERT INTO reflections (id, repo, text, created_at) \
                 VALUES ('old-row', 'legion', 'pre-migration row', '2026-01-01T00:00:00+00:00')",
                [],
            )
            .unwrap();
        }

        let db = Database::open(&path).unwrap();

        // Migration 14 backfill ran: updated_at seeded from created_at.
        let updated: Option<String> = db
            .conn
            .query_row(
                "SELECT updated_at FROM reflections WHERE id = 'old-row'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(updated.as_deref(), Some("2026-01-01T00:00:00+00:00"));

        // The current full-column write and read paths work on the
        // migrated database.
        let r = db
            .insert_reflection("legion", "post-migration row", "team")
            .unwrap();
        assert!(db.get_reflection_by_id(&r.id).unwrap().is_some());
    }

    #[test]
    fn init_schema_migrates_a_pre_revision_documents_table() {
        // A `documents` table created before the `revision` column existed
        // (#882 step 1) must come up through the migration with existing
        // rows backfilled to revision 1, and the write/read paths that
        // depend on the column working afterward.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("old.db");
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(
                "CREATE TABLE documents (
                    id TEXT PRIMARY KEY,
                    type TEXT NOT NULL,
                    surface TEXT,
                    status TEXT NOT NULL DEFAULT 'draft',
                    priority TEXT,
                    owner TEXT NOT NULL,
                    payload TEXT NOT NULL,
                    archived_at TEXT,
                    created_at TEXT NOT NULL,
                    updated_at TEXT NOT NULL,
                    deleted_at TEXT
                );",
            )
            .unwrap();
            conn.execute(
                "INSERT INTO documents (id, type, owner, payload, created_at, updated_at) \
                 VALUES ('old-doc', 'requirement', 'legion', '{}', \
                 '2026-01-01T00:00:00+00:00', '2026-01-01T00:00:00+00:00')",
                [],
            )
            .unwrap();
        }

        let db = Database::open(&path).unwrap();
        crate::db::testutil::seed_type_schema(&db, "requirement");

        // Pre-existing row backfilled to revision 1 by the column DEFAULT.
        assert_eq!(db.document_revision("old-doc").unwrap(), 1);

        // revise_document (which depends on the column existing) works on
        // the migrated database.
        let revised = db.revise_document("old-doc", "{}").unwrap();
        assert_eq!(revised.id, "old-doc");
        assert_eq!(db.document_revision("old-doc").unwrap(), 2);
    }

    #[test]
    fn open_sets_default_busy_timeout() {
        let db = test_db();
        let timeout_ms: i64 = db
            .conn
            .pragma_query_value(None, "busy_timeout", |row| row.get(0))
            .unwrap();
        assert_eq!(timeout_ms, DEFAULT_BUSY_TIMEOUT.as_millis() as i64);
    }

    #[test]
    fn concurrent_open_and_write_retries_instead_of_immediate_busy() {
        // Behavioral guard for #721: a second connection opening and
        // writing while another holds the write lock retries rather than
        // failing instantly, succeeding once the first connection commits.
        // (`open_sets_default_busy_timeout` above is the guard that pins the
        // exact 2s value -- rusqlite's bundled sqlite already applies its
        // own 5s busy_timeout default on every `Connection::open`, so this
        // test alone can't distinguish "our 2s" from "rusqlite's built-in
        // 5s"; it exists to prove the concurrent open+write path actually
        // retries in practice, per #721's acceptance criteria.)
        use std::sync::{Arc, Barrier};
        use std::thread;
        use std::time::Instant;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("busy.db");

        // Create the file and run schema init up front via a throwaway
        // connection so both threads below open an already-migrated db.
        drop(Database::open(&path).unwrap());

        let lock_held = Arc::new(Barrier::new(2));
        let lock_held_writer = Arc::clone(&lock_held);
        let writer_path = path.clone();

        let holder = thread::spawn(move || {
            let db = Database::open(&writer_path).unwrap();
            db.conn.execute_batch("BEGIN IMMEDIATE;").unwrap();
            db.conn
                .execute(
                    "INSERT INTO reflections (id, repo, text, created_at) \
                     VALUES ('holder-row', 'legion', 'held by writer', '2026-01-01T00:00:00+00:00')",
                    [],
                )
                .unwrap();
            // Signal the write lock is held before sleeping, so the main
            // thread's write below is guaranteed to contend for it.
            lock_held_writer.wait();
            thread::sleep(Duration::from_millis(300));
            db.conn.execute_batch("COMMIT;").unwrap();
        });

        lock_held.wait();

        // Time the open AND the write together. The store is already
        // stamped, so open skips the migration chain (#1289) and the
        // contention surfaces at the insert.
        let start = Instant::now();
        let waiter = Database::open(&path).unwrap();
        waiter
            .insert_reflection("legion", "waited for the write lock", "self")
            .expect("write should retry past SQLITE_BUSY and succeed");
        let elapsed = start.elapsed();

        holder.join().unwrap();

        // The write only succeeds once the holder commits at ~300ms, so a
        // near-instant elapsed time here would mean the busy timeout was
        // not actually applied (i.e. it failed fast instead of retrying).
        assert!(
            elapsed >= Duration::from_millis(100),
            "expected the writer to block on the lock and retry, took {elapsed:?}"
        );
    }

    #[test]
    fn partial_indexes_created_for_soft_delete() {
        let db = test_db();

        // Query sqlite_master for our partial indexes.
        let mut stmt = db
            .conn
            .prepare("SELECT name FROM sqlite_master WHERE type = 'index' AND name LIKE '%_live'")
            .unwrap();
        let indexes: Vec<String> = stmt
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap();

        // Verify all expected partial indexes exist.
        assert!(
            indexes.contains(&"idx_reflections_repo_live".to_string()),
            "idx_reflections_repo_live should exist"
        );
        assert!(
            indexes.contains(&"idx_reflections_audience_live".to_string()),
            "idx_reflections_audience_live should exist"
        );
        assert!(
            indexes.contains(&"idx_tasks_to_live".to_string()),
            "idx_tasks_to_live should exist"
        );
        assert!(
            indexes.contains(&"idx_tasks_from_live".to_string()),
            "idx_tasks_from_live should exist"
        );
        assert!(
            indexes.contains(&"idx_schedules_repo_live".to_string()),
            "idx_schedules_repo_live should exist"
        );
    }

    #[test]
    fn rename_repo_carries_watch_redelivery_and_avoids_pk_collision() {
        // #948: watch_redelivery must move with the repo exactly like
        // watch_handled -- delete the target's rows first (composite PK
        // collision avoidance), then rename the source's rows in.
        let db = test_db();
        db.conn
            .execute(
                "INSERT INTO watch_redelivery (signal_id, repo_name, attempts, last_failed_at) \
                 VALUES ('sig-a', 'old-name', 2, '2026-01-01T00:00:00+00:00')",
                [],
            )
            .unwrap();
        // A pre-existing row under the target name (any signal_id) must be
        // wiped before the rename -- mirroring watch_handled's own
        // collision-avoidance, this is a wholesale delete of the target's
        // existing rows, not a selective one keyed on signal_id.
        db.conn
            .execute(
                "INSERT INTO watch_redelivery (signal_id, repo_name, attempts, last_failed_at) \
                 VALUES ('sig-b', 'new-name', 1, '2026-01-01T00:00:00+00:00')",
                [],
            )
            .unwrap();

        let counts = db.rename_repo("old-name", "new-name").unwrap();
        assert_eq!(counts.watch_redelivery, 1, "one row renamed from old-name");
        assert!(
            counts.total() >= counts.watch_redelivery,
            "watch_redelivery must count toward the total"
        );

        let rows: Vec<(String, i64)> = db
            .conn
            .prepare("SELECT signal_id, attempts FROM watch_redelivery WHERE repo_name = 'new-name' ORDER BY signal_id")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(
            rows,
            vec![("sig-a".to_string(), 2)],
            "the target's pre-existing row must be wiped before the rename, \
             leaving only the row carried over from the source name"
        );

        let old_count: i64 = db
            .conn
            .query_row(
                "SELECT COUNT(*) FROM watch_redelivery WHERE repo_name = 'old-name'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(old_count, 0, "no rows should remain under the old name");
    }

    /// Every schema object in the store, ordered, as `(type, name, sql)`.
    /// Two stores with equal snapshots carry the same tables, columns, and
    /// indexes (an ALTER ... ADD COLUMN rewrites the table's stored `sql`).
    type SchemaSnapshot = Vec<(String, String, Option<String>)>;

    fn schema_snapshot(conn: &Connection) -> SchemaSnapshot {
        conn.prepare("SELECT type, name, sql FROM sqlite_master ORDER BY type, name")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .unwrap()
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap()
    }

    /// The schema a fresh store gets from one uncontended open.
    fn reference_schema() -> SchemaSnapshot {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::open(&dir.path().join("reference.db")).unwrap();
        schema_snapshot(&db.conn)
    }

    /// Assert the store at `path` carries the full schema and the stamp.
    fn assert_full_schema(path: &Path, reference: &SchemaSnapshot) {
        let conn = Connection::open(path).unwrap();
        assert_eq!(&schema_snapshot(&conn), reference, "schema incomplete");
        assert_eq!(
            Database::schema_version(&conn).unwrap(),
            SCHEMA_VERSION,
            "schema version unstamped"
        );
    }

    /// SHA-256 of a fresh store's [`schema_snapshot`] at [`SCHEMA_VERSION`].
    const PINNED_SCHEMA_FINGERPRINT: (i32, &str) = (
        1,
        "8acd6b6d4eb1cdb89f70157436f5e2b63e4ad48091edcdd3a51888f2df5895bd",
    );

    #[test]
    fn schema_fingerprint_is_pinned_to_schema_version() {
        // #1289: an already-stamped store skips the migration chain, so a
        // schema change that does not bump SCHEMA_VERSION would never reach
        // it. Any change to the fresh schema changes this fingerprint.
        use sha2::{Digest, Sha256};

        let snapshot = reference_schema();
        let fingerprint = hex::encode(Sha256::digest(format!("{snapshot:?}")));
        assert_eq!(
            (SCHEMA_VERSION, fingerprint.as_str()),
            PINNED_SCHEMA_FINGERPRINT,
            "the fresh schema changed: bump SCHEMA_VERSION in src/db/mod.rs and \
             re-pin PINNED_SCHEMA_FINGERPRINT to (new version, new fingerprint)"
        );
    }

    #[test]
    fn open_stamps_schema_version_and_skips_the_chain_when_current() {
        // #1289: the full chain stamps the store; a later open finds the
        // stamp and returns without taking the write lock, so it succeeds
        // even while another connection holds that lock.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("stamped.db");
        drop(Database::open(&path).unwrap());

        let holder = Connection::open(&path).unwrap();
        assert_eq!(Database::schema_version(&holder).unwrap(), SCHEMA_VERSION);
        holder.execute_batch("BEGIN IMMEDIATE;").unwrap();
        let start = std::time::Instant::now();
        Database::open(&path).unwrap();
        assert!(
            start.elapsed() < DEFAULT_BUSY_TIMEOUT / 2,
            "a current store's open waited on the write lock"
        );
        holder.execute_batch("COMMIT;").unwrap();
    }

    const CONCURRENT_OPENERS: usize = 8;

    #[test]
    fn concurrent_thread_opens_of_a_fresh_store_all_succeed() {
        // #1289: N threads released together at one fresh store must all
        // open it, and the store must end with the full schema. Repeated
        // over many fresh stores so an unserialized migration chain would
        // race (duplicate column name) in at least one round.
        use std::sync::{Arc, Barrier};
        use std::thread;

        const ROUNDS: usize = 40;
        let reference = reference_schema();
        for round in 0..ROUNDS {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("fresh.db");
            let barrier = Arc::new(Barrier::new(CONCURRENT_OPENERS));
            let handles: Vec<_> = (0..CONCURRENT_OPENERS)
                .map(|_| {
                    let barrier = Arc::clone(&barrier);
                    let path = path.clone();
                    thread::spawn(move || {
                        barrier.wait();
                        Database::open(&path).map(drop)
                    })
                })
                .collect();
            for handle in handles {
                if let Err(e) = handle.join().unwrap() {
                    panic!("round {round}: concurrent open failed: {e}");
                }
            }
            assert_full_schema(&path, &reference);
        }
    }

    /// Env var naming the store a child process opens in
    /// `concurrent_open_child`; unset in a normal test run.
    const CHILD_DB_ENV: &str = "LEGION_TEST_1289_CHILD_DB";
    /// Env var naming the file whose appearance releases every child at once.
    const CHILD_GO_ENV: &str = "LEGION_TEST_1289_CHILD_GO";
    /// Line a child prints after its open succeeds, so the parent can tell
    /// a real open from a test filter that matched nothing.
    const CHILD_SENTINEL: &str = "legion-1289-child-opened";

    /// Child side of `concurrent_process_opens_of_a_fresh_store_all_succeed`.
    /// A no-op pass unless the parent set [`CHILD_DB_ENV`].
    #[test]
    fn concurrent_open_child() {
        let Some(db_path) = std::env::var_os(CHILD_DB_ENV) else {
            return;
        };
        let go = std::path::PathBuf::from(std::env::var_os(CHILD_GO_ENV).unwrap());
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        while !go.exists() {
            assert!(
                std::time::Instant::now() < deadline,
                "go file never appeared"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
        Database::open(Path::new(&db_path)).unwrap();
        println!("{CHILD_SENTINEL}");
    }

    #[test]
    fn concurrent_process_opens_of_a_fresh_store_all_succeed() {
        // #1289: N separate processes opening one fresh store at once must
        // all succeed and leave the full schema. Each child is this test
        // binary running `concurrent_open_child`; all children wait on a go
        // file so their opens overlap instead of trailing process startup.
        use std::process::{Command, Stdio};

        const ROUNDS: usize = 8;
        let exe = std::env::current_exe().unwrap();
        let child_test = format!(
            "{}::concurrent_open_child",
            module_path!().split_once("::").unwrap().1
        );
        let reference = reference_schema();
        for round in 0..ROUNDS {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("fresh.db");
            let go = dir.path().join("go");
            let children: Vec<_> = (0..CONCURRENT_OPENERS)
                .map(|_| {
                    Command::new(&exe)
                        .args([child_test.as_str(), "--exact", "--nocapture"])
                        .env(CHILD_DB_ENV, &path)
                        .env(CHILD_GO_ENV, &go)
                        .stdout(Stdio::piped())
                        .stderr(Stdio::piped())
                        .spawn()
                        .unwrap()
                })
                .collect();
            std::fs::write(&go, b"").unwrap();
            for child in children {
                let out = child.wait_with_output().unwrap();
                let stdout = String::from_utf8_lossy(&out.stdout);
                assert!(
                    out.status.success() && stdout.contains(CHILD_SENTINEL),
                    "round {round}: child open failed\nstdout:\n{stdout}\nstderr:\n{}",
                    String::from_utf8_lossy(&out.stderr)
                );
            }
            assert_full_schema(&path, &reference);
        }
    }
}
