//! Shared temp-database helper for db domain tests (#609).
//!
//! Converges the scattered per-file `test_db` helpers (db.rs, documents.rs,
//! stats.rs, uncertainty/storage.rs) on one definition.

use super::Database;

/// Create a database backed by a fresh tempdir for testing.
///
/// The `TempDir` is deliberately leaked via `std::mem::forget` so the backing
/// directory outlives the returned `Database`. Dropping it here would unlink
/// the db/-wal/-shm files while the connection is live, leaving the database
/// working only through unlinked-open-fd semantics -- and any test that
/// reopens the path, opens a second connection, or checkpoints would fail
/// mysteriously. Tests that need to reopen the database by path must bind
/// their own tempdir instead of using this helper (see the v1 migration test
/// in `db/mod.rs`). The leak is bounded: one small directory per test, and
/// the OS reclaims the temp filesystem location later.
pub(crate) fn test_db() -> Database {
    let dir = tempfile::tempdir().unwrap();
    let db = Database::open(&dir.path().join("test.db")).unwrap();
    std::mem::forget(dir);
    db
}
