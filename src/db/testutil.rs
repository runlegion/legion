//! Shared temp-database helper for db domain tests (#609).
//!
//! Converges the scattered per-file `test_db` helpers (db.rs, documents.rs,
//! stats.rs, uncertainty/storage.rs) on one definition.

use super::Database;

/// Create a database backed by a fresh tempdir for testing.
pub(crate) fn test_db() -> Database {
    let dir = tempfile::tempdir().unwrap();
    Database::open(&dir.path().join("test.db")).unwrap()
}
