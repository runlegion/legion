use std::path::{Path, PathBuf};

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

/// Suite-wide isolated (always-empty) `GIT_CONFIG_GLOBAL`/`GIT_CONFIG_SYSTEM`
/// file paths for any test's fixture git invocations (shared by
/// `cli::index_cmd`'s worktree-divergence tests and `cmd::hook`'s
/// worktree-resolution test, previously two separate copies of the same
/// helper). Without this, a fixture's `-c` overrides only pin
/// identity/signing -- config *values* like a developer's real
/// `core.hooksPath`, `init.defaultBranch`, or a broken `commit.gpgsign`
/// signer would still leak in from `~/.gitconfig`. The backing tempdir is
/// deliberately leaked: it must outlive every test in this binary, and
/// process exit reclaims it like any other tempfile.
pub fn isolated_git_config_paths() -> &'static (PathBuf, PathBuf) {
    static ISOLATED_GIT_CONFIG: std::sync::OnceLock<(PathBuf, PathBuf)> =
        std::sync::OnceLock::new();
    ISOLATED_GIT_CONFIG.get_or_init(|| {
        let dir = tempfile::tempdir().expect("create isolated git config dir");
        let global = dir.path().join("global.gitconfig");
        let system = dir.path().join("system.gitconfig");
        std::fs::write(&global, "").expect("write isolated global gitconfig");
        std::fs::write(&system, "").expect("write isolated system gitconfig");
        std::mem::forget(dir);
        (global, system)
    })
}

/// Run `git` in `dir` with the isolated config above (never a real `git
/// config` write, never the developer's real `~/.gitconfig`, and
/// `commit.gpgsign=false` explicit so a fixture commit never depends on a
/// real signer -- this environment's happens to be broken). Plain
/// directory-based discovery only (`current_dir`, no `GIT_DIR`/
/// `GIT_WORK_TREE` override): those would be hardcoded to `<dir>/.git`,
/// which is a plain FILE (not a directory) inside a linked worktree,
/// breaking any fixture that creates one.
pub fn git_in(dir: &Path, args: &[&str]) {
    let (global, system) = isolated_git_config_paths();
    let mut full_args: Vec<&str> = vec![
        "-c",
        "user.name=Legion Test Fixture",
        "-c",
        "user.email=legion-test-fixture@example.invalid",
        "-c",
        "commit.gpgsign=false",
    ];
    full_args.extend_from_slice(args);
    let out = std::process::Command::new("git")
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", global)
        .env("GIT_CONFIG_SYSTEM", system)
        .args(&full_args)
        .output()
        .unwrap_or_else(|e| panic!("git {args:?} failed to spawn in {dir:?}: {e}"));
    assert!(
        out.status.success(),
        "git {args:?} exited non-zero in {dir:?}\nstderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}
