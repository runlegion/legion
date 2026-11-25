//! Canonical data-dir resolution and one-time data migrations
//! (carved from main.rs, #610). Re-exported at the crate root as
//! `crate::data_dir` so external callers are unchanged.

use std::path::{Path, PathBuf};

use directories::ProjectDirs;

use crate::error;

/// Resolve the legion data directory.
///
/// Resolution order:
/// 1. `LEGION_DATA_DIR` env var (explicit override, used by tests)
/// 2. `ProjectDirs` platform-native default:
///    - macOS:   `~/Library/Application Support/legion/`
///    - Linux:   `~/.local/share/legion/`
///    - Windows: `%APPDATA%\legion\`
///
/// `CLAUDE_PLUGIN_DATA` is deliberately NOT consulted. A prior version of
/// this function preferred that env var, which caused a split-brain on
/// macOS: the Claude Code plugin context wrote to
/// `~/.claude/plugins/data/legion-legion/` while bare CLI invocations and
/// long-running `legion watch` processes wrote to the ProjectDirs path.
/// Two divergent databases, invisible to each other, silently broke the
/// agent learning loop (reflections in one DB were invisible to sessions
/// that opened the other). The single-path rule is load-bearing for
/// legion's memory integrity. Do not reintroduce a second default.
///
/// Result is cached via OnceLock after first successful resolution:
/// commands like `audit()` re-enter this dozens of times and the path
/// never changes inside a single process. Tests that manipulate
/// `LEGION_DATA_DIR` between calls should use a fresh subprocess.
///
/// On the first successful resolution, if the target has no `legion.db`
/// but the legacy plugin-data-dir location does, the database, Tantivy
/// index, and `watch.toml` are migrated across. One-way, best-effort,
/// leaves the legacy path in place as a safety net. This is the reverse
/// of the previous migration direction, which moved data INTO the plugin
/// data dir and caused the split-brain in the first place.
pub(crate) fn data_dir() -> error::Result<PathBuf> {
    use std::sync::OnceLock;
    static CACHED: OnceLock<PathBuf> = OnceLock::new();

    if let Some(cached) = CACHED.get() {
        return Ok(cached.clone());
    }

    // `via_override` tracks whether the caller explicitly set LEGION_DATA_DIR.
    // When true (tests, CI, or explicit user override), the plugin-data-dir
    // migration MUST be skipped -- otherwise a test tempdir target would
    // inherit content from the real user's plugin data dir on the host
    // machine and tests that expect a clean starting state would fail
    // unpredictably depending on whose box they ran on. The override is the
    // explicit "I know what I'm doing, stay out of the filesystem" signal.
    let (path, via_override) = if let Ok(dir) = std::env::var("LEGION_DATA_DIR") {
        (PathBuf::from(dir), true)
    } else {
        let p = ProjectDirs::from("", "", "legion")
            .ok_or(error::LegionError::NoHomeDir)?
            .data_dir()
            .to_path_buf();
        (p, false)
    };

    std::fs::create_dir_all(&path)?;

    if !via_override && !path.join("legion.db").exists() {
        migrate_from_plugin_data_dir(&path);
    }

    let _ = CACHED.set(path.clone());
    Ok(path)
}

/// Migrate legion state from the plugin data dir back to the ProjectDirs target.
///
/// Reverse of the previous migration direction. The prior version moved
/// ProjectDirs -> plugin data dir, which created the split-brain this
/// module now exists to clean up. This function copies the OTHER way,
/// recovering any user whose plugin data dir has their authoritative
/// legion state but whose ProjectDirs path was empty.
///
/// Resolves the plugin data dir from `CLAUDE_PLUGIN_DATA` if set (when
/// running under Claude Code), otherwise from the known fallback path
/// `$HOME/.claude/plugins/data/legion-legion/`. Delegates the copy to
/// [`migrate_between`], which is the testable inner form.
fn migrate_from_plugin_data_dir(target: &Path) {
    let source = if let Ok(dir) = std::env::var("CLAUDE_PLUGIN_DATA")
        && !dir.is_empty()
    {
        PathBuf::from(dir)
    } else if let Some(home) = dirs::home_dir() {
        home.join(".claude")
            .join("plugins")
            .join("data")
            .join("legion-legion")
    } else {
        return;
    };
    migrate_between(&source, target);
}

/// Copy legion state from `legacy` to `target`.
///
/// Runs only when `legacy/legion.db` exists and `target/legion.db` does not.
/// Best-effort: logs to stderr on each failure and continues. Moves
/// `legion.db` (after a WAL checkpoint on the source so the copy is
/// consistent), `watch.toml`, and the Tantivy `index/` directory tree. Does
/// NOT delete the source -- leaves it in place as a safety net until the
/// user cleans up manually.
///
/// The Tantivy index is copied BEFORE `legion.db` so a mid-migration failure
/// leaves the target without a DB, which causes the next run to retry the
/// whole migration from scratch rather than strand a populated DB with an
/// empty index.
fn migrate_between(legacy: &Path, target: &Path) {
    let legacy_db = legacy.join("legion.db");
    if !legacy_db.exists() {
        return;
    }
    if target.join("legion.db").exists() {
        return;
    }

    eprintln!(
        "[legion] first-run migration: copying state from {} to {}",
        legacy.display(),
        target.display()
    );

    // 1. Tantivy index first: if this fails, the target still has no
    //    legion.db so the next run retries the full migration.
    let legacy_index = legacy.join("index");
    let target_index = target.join("index");
    if legacy_index.is_dir()
        && !target_index.exists()
        && let Err(e) = copy_dir_recursive(&legacy_index, &target_index)
    {
        eprintln!(
            "[legion] migration: failed to copy index tree: {e} -- aborting before DB copy so the next run retries"
        );
        return;
    }

    // 2. WAL checkpoint on the source so the DB copy is internally
    //    consistent. Opening the legacy DB briefly and issuing
    //    PRAGMA wal_checkpoint(TRUNCATE) flushes any pending writes into the
    //    main file and empties the WAL. If the checkpoint fails (e.g. locked
    //    by a running watch daemon), we log and proceed with the copy anyway;
    //    the target may need a manual recovery but this is no worse than the
    //    previous behavior.
    if let Err(e) = checkpoint_sqlite(&legacy_db) {
        eprintln!("[legion] migration: WAL checkpoint failed ({e}); copy may be inconsistent");
    }

    // 3. Copy the main DB last. After this point the target is "owned" and
    //    the migration guard (`target/legion.db.exists()`) will prevent re-runs.
    if let Err(e) = std::fs::copy(&legacy_db, target.join("legion.db")) {
        eprintln!("[legion] migration: failed to copy legion.db: {e}");
        return;
    }

    // 4. watch.toml: optional, best-effort.
    let legacy_watch = legacy.join("watch.toml");
    if legacy_watch.exists()
        && let Err(e) = std::fs::copy(&legacy_watch, target.join("watch.toml"))
    {
        eprintln!("[legion] migration: failed to copy watch.toml: {e}");
    }

    eprintln!(
        "[legion] migration complete. Legacy path left in place: {}",
        legacy.display()
    );
    eprintln!(
        "[legion] delete manually once verified: rm -rf {}",
        legacy.display()
    );
}

/// Open the given SQLite file briefly and issue a TRUNCATE checkpoint.
///
/// Used by [`migrate_between`] to make sure the WAL is drained into the main
/// file before the copy. Returns the rusqlite error on failure; caller decides
/// whether to proceed with the copy anyway.
fn checkpoint_sqlite(path: &Path) -> std::result::Result<(), rusqlite::Error> {
    let conn = rusqlite::Connection::open(path)?;
    conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")?;
    Ok(())
}

/// Recursively copy a directory tree. Used for the Tantivy index migration.
/// Follows symlinks (the legacy index is expected to be a normal directory
/// tree with no symlinks); if that ever changes, switch to `symlink_metadata`
/// and preserve the link type.
fn copy_dir_recursive(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let ty = entry.file_type()?;
        let dst_path = dst.join(entry.file_name());
        if ty.is_dir() {
            copy_dir_recursive(&entry.path(), &dst_path)?;
        } else {
            std::fs::copy(entry.path(), dst_path)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod migration_tests {
    use super::*;

    fn seed_legacy_db(legacy: &Path) {
        std::fs::create_dir_all(legacy).expect("create legacy dir");
        let conn = rusqlite::Connection::open(legacy.join("legion.db")).expect("open legacy db");
        conn.execute_batch(
            "CREATE TABLE sentinel (value TEXT NOT NULL); \
             INSERT INTO sentinel VALUES ('migrated');",
        )
        .expect("seed legacy db");
    }

    #[test]
    fn migrate_between_skips_when_legacy_has_no_db() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let legacy = tmp.path().join("legacy");
        let target = tmp.path().join("target");
        std::fs::create_dir_all(&legacy).unwrap();
        std::fs::create_dir_all(&target).unwrap();

        migrate_between(&legacy, &target);
        assert!(
            !target.join("legion.db").exists(),
            "target should stay empty when legacy has no db"
        );
    }

    #[test]
    fn migrate_between_skips_when_target_already_has_db() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let legacy = tmp.path().join("legacy");
        let target = tmp.path().join("target");
        seed_legacy_db(&legacy);
        std::fs::create_dir_all(&target).unwrap();
        std::fs::write(target.join("legion.db"), b"existing").unwrap();

        migrate_between(&legacy, &target);
        // Target DB should remain the placeholder bytes, not the legacy content.
        let content = std::fs::read(target.join("legion.db")).unwrap();
        assert_eq!(content, b"existing", "migration should not overwrite");
    }

    #[test]
    fn migrate_between_copies_db_and_index() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let legacy = tmp.path().join("legacy");
        let target = tmp.path().join("target");
        seed_legacy_db(&legacy);

        // Seed a fake Tantivy-style index tree with nested files.
        std::fs::create_dir_all(legacy.join("index/segment_0")).unwrap();
        std::fs::write(legacy.join("index/meta.json"), b"{\"version\": 1}").unwrap();
        std::fs::write(
            legacy.join("index/segment_0/posting.bin"),
            b"\x00\x01\x02\x03",
        )
        .unwrap();

        // Seed watch.toml
        std::fs::write(legacy.join("watch.toml"), b"[[repos]]\nname = \"legion\"\n").unwrap();

        std::fs::create_dir_all(&target).unwrap();
        migrate_between(&legacy, &target);

        assert!(target.join("legion.db").exists(), "db not migrated");
        assert!(
            target.join("index/meta.json").exists(),
            "index meta not migrated"
        );
        assert!(
            target.join("index/segment_0/posting.bin").exists(),
            "nested index file not migrated"
        );
        assert!(
            target.join("watch.toml").exists(),
            "watch.toml not migrated"
        );

        // Legacy must remain in place as a safety net.
        assert!(
            legacy.join("legion.db").exists(),
            "legacy db removed (should be kept)"
        );

        // Migrated DB should be readable and contain the sentinel row.
        let conn = rusqlite::Connection::open(target.join("legion.db")).unwrap();
        let value: String = conn
            .query_row("SELECT value FROM sentinel", [], |row| row.get(0))
            .unwrap();
        assert_eq!(value, "migrated");
    }

    #[test]
    fn migrate_between_is_idempotent_on_rerun() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let legacy = tmp.path().join("legacy");
        let target = tmp.path().join("target");
        seed_legacy_db(&legacy);
        std::fs::create_dir_all(&target).unwrap();

        migrate_between(&legacy, &target);
        // Second call should be a no-op because target now has legion.db.
        let first_content = std::fs::read(target.join("legion.db")).unwrap();
        migrate_between(&legacy, &target);
        let second_content = std::fs::read(target.join("legion.db")).unwrap();
        assert_eq!(first_content, second_content);
    }

    #[test]
    fn copy_dir_recursive_handles_nested_tree() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let src = tmp.path().join("src");
        let dst = tmp.path().join("dst");
        std::fs::create_dir_all(src.join("a/b/c")).unwrap();
        std::fs::write(src.join("top.txt"), b"top").unwrap();
        std::fs::write(src.join("a/mid.txt"), b"mid").unwrap();
        std::fs::write(src.join("a/b/leaf.bin"), b"\xde\xad\xbe\xef").unwrap();

        copy_dir_recursive(&src, &dst).expect("copy");

        assert_eq!(std::fs::read(dst.join("top.txt")).unwrap(), b"top");
        assert_eq!(std::fs::read(dst.join("a/mid.txt")).unwrap(), b"mid");
        assert_eq!(
            std::fs::read(dst.join("a/b/leaf.bin")).unwrap(),
            b"\xde\xad\xbe\xef"
        );
        assert!(dst.join("a/b/c").is_dir(), "empty nested dir not created");
    }

    #[test]
    fn checkpoint_sqlite_drains_wal() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let db_path = tmp.path().join("legion.db");
        {
            let conn = rusqlite::Connection::open(&db_path).unwrap();
            conn.execute_batch(
                "PRAGMA journal_mode = WAL; \
                 CREATE TABLE t (x INTEGER); \
                 INSERT INTO t VALUES (1), (2), (3);",
            )
            .unwrap();
        }
        // WAL may be non-empty at this point.
        checkpoint_sqlite(&db_path).expect("checkpoint should succeed");
        // Data must still be readable after the checkpoint.
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM t", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 3);
    }
}
