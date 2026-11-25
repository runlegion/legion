use crate::db::Database;
use crate::search::SearchIndex;

/// Create a Database and SearchIndex backed by a single temporary directory.
///
/// Returns both handles and the TempDir. The TempDir must outlive the
/// handles to keep the underlying files accessible.
pub fn test_storage() -> (Database, SearchIndex, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("failed to create tempdir");
    let db = Database::open(&dir.path().join("test.db")).expect("failed to open database");
    let index = SearchIndex::open(&dir.path().join("index")).expect("failed to open search index");
    (db, index, dir)
}
