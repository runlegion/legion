//! File inventory walk: enumerate every non-ignored file in a repo (#705).
//!
//! `walk_repo` drives an `ignore::WalkBuilder` over a watched workdir and
//! returns one [`FileInventoryEntry`] per file. The walk respects `.gitignore`
//! rules even when the root is not a git checkout (`require_git(false)`), so
//! tempdir-based tests are not vacuously "passing" by ignoring nothing.
//!
//! `lang_for_ext` maps file extensions to the SCIP language tags used by
//! `legion index`. Files whose extension has no mapping land with `lang = None`.

use std::path::Path;

use chrono::{DateTime, Utc};
use ignore::WalkBuilder;

use crate::db::inventory::FileInventoryEntry;

/// Map a file extension (without the leading dot) to a SCIP language tag.
///
/// Returns `None` for extensions that no SCIP indexer covers. The mapping
/// mirrors the languages detected by `scip::detect_languages`.
pub fn lang_for_ext(ext: &str) -> Option<&'static str> {
    match ext {
        "rs" => Some("rust"),
        "ts" | "tsx" | "js" | "jsx" | "mts" | "cts" | "mjs" | "cjs" => Some("typescript"),
        "py" | "pyi" => Some("python"),
        "go" => Some("go"),
        "java" | "kt" | "kts" => Some("java"),
        "rb" => Some("ruby"),
        "c" | "h" | "cc" | "cpp" | "cxx" | "hh" | "hpp" | "hxx" => Some("clang"),
        "cs" => Some("csharp"),
        "php" => Some("php"),
        _ => None,
    }
}

/// What a repo walk produced, including how much it could not see.
///
/// `walk_errors` counts walk-level failures (unreadable directories,
/// readdir errors, per-file stat failures) that were skipped. Nonzero
/// means `entries` may be INCOMPLETE -- callers that evict rows based on
/// the entry set (the prune pass) must not treat a partial walk as the
/// full corpus, or a transient I/O failure evicts rows for files that
/// still exist (#718 re-review).
pub struct WalkOutcome {
    pub entries: Vec<FileInventoryEntry>,
    pub walk_errors: u64,
}

/// Walk `repo_path` and return one inventory entry per non-ignored file,
/// plus the count of walk errors encountered.
///
/// The walk respects `.gitignore` rules via the `ignore` crate, including
/// rules from parent directories inside the repo. Settings that could vary
/// across machines (global gitignore, `core.excludesFile`) are explicitly
/// disabled so the inventory is reproducible.
///
/// Dotfiles and dot-directories are included (`hidden(false)`): files like
/// `.github/workflows/ci.yml`, `.env.example`, and `.gitignore` itself are
/// part of the repo corpus and must appear in the inventory. With hidden
/// disabled the `ignore` crate does NOT skip `.git/` on its own (verified
/// empirically by `git_dir_is_excluded_even_with_hidden_false`), so the
/// walk excludes it explicitly via `filter_entry`.
///
/// `require_git` is set to `false` so the walk works correctly both inside
/// a git checkout and in plain directories (tempdir-based tests). Per-file
/// stat/walk errors are logged to stderr, skipped, and counted in
/// `walk_errors` -- one unreadable file must not abort the whole inventory,
/// but the caller must know the result is partial.
///
/// Returned paths are repo-relative with forward-slash separators and no
/// leading slash, matching the convention used by the SCIP document paths.
///
/// Note: if `metadata().modified()` is unavailable (rare on some platforms),
/// the mtime falls back silently to `Utc::now()`. The row then looks fresh
/// on every index until a platform with mtime support updates it -- an
/// accepted inaccuracy on exotic filesystems, not worth a per-file log line.
pub fn walk_repo(repo_name: &str, repo_path: &Path) -> WalkOutcome {
    let walker = WalkBuilder::new(repo_path)
        .require_git(false)
        .hidden(false)
        .git_global(false)
        .filter_entry(|entry| entry.file_name() != ".git")
        .build();
    let mut entries: Vec<FileInventoryEntry> = Vec::new();
    let mut walk_errors: u64 = 0;

    for result in walker {
        let dir_entry = match result {
            Ok(e) => e,
            Err(err) => {
                eprintln!("[legion] inventory walk error (skipped): {err}");
                walk_errors += 1;
                continue;
            }
        };

        // Skip directories and symlinks -- files only.
        if !dir_entry.file_type().is_some_and(|ft| ft.is_file()) {
            continue;
        }

        let abs_path = dir_entry.path();

        let metadata = match abs_path.metadata() {
            Ok(m) => m,
            Err(err) => {
                eprintln!(
                    "[legion] inventory stat error for {}: {err} (skipped)",
                    abs_path.display()
                );
                walk_errors += 1;
                continue;
            }
        };

        let size: u64 = metadata.len();
        let mtime: DateTime<Utc> = metadata
            .modified()
            .map(|t| t.into())
            .unwrap_or_else(|_| Utc::now());
        let mtime_str: String = mtime.to_rfc3339();

        // Repo-relative path with forward slashes.
        let rel = abs_path
            .strip_prefix(repo_path)
            .unwrap_or(abs_path)
            .to_string_lossy()
            .replace('\\', "/");

        let ext: Option<String> = abs_path
            .extension()
            .and_then(|e| e.to_str())
            .map(|s| s.to_string());

        let lang: Option<String> = ext.as_deref().and_then(lang_for_ext).map(|s| s.to_string());

        entries.push(FileInventoryEntry {
            repo: repo_name.to_string(),
            path: rel,
            ext,
            lang,
            size,
            mtime: mtime_str,
            symbol_count: 0,
        });
    }

    WalkOutcome {
        entries,
        walk_errors,
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::TempDir;

    use super::*;

    // Helper: create a temp dir with the given relative file paths (empty
    // content), return the dir so it stays alive for the duration of the test.
    fn make_tree(files: &[&str]) -> TempDir {
        let dir = tempfile::tempdir().unwrap();
        for rel in files {
            let abs = dir.path().join(rel);
            if let Some(parent) = abs.parent() {
                fs::create_dir_all(parent).unwrap();
            }
            fs::write(&abs, b"").unwrap();
        }
        dir
    }

    // --- lang_for_ext mapping ---

    #[test]
    fn lang_for_ext_rust() {
        assert_eq!(lang_for_ext("rs"), Some("rust"));
    }

    #[test]
    fn lang_for_ext_typescript_variants() {
        for ext in ["ts", "tsx", "js", "jsx", "mts", "cts"] {
            assert_eq!(
                lang_for_ext(ext),
                Some("typescript"),
                "ext={ext} should map to typescript"
            );
        }
    }

    #[test]
    fn lang_for_ext_unknown_returns_none() {
        assert_eq!(lang_for_ext("sh"), None);
        assert_eq!(lang_for_ext("md"), None);
        assert_eq!(lang_for_ext("toml"), None);
        assert_eq!(lang_for_ext("yaml"), None);
        assert_eq!(lang_for_ext("css"), None);
    }

    // --- walk_repo: one row per non-ignored file ---

    #[test]
    fn walk_produces_one_row_per_file() {
        let dir = make_tree(&["src/main.rs", "README.md", "Cargo.toml"]);
        let outcome = walk_repo("myrepo", dir.path());
        assert_eq!(
            outcome.entries.len(),
            3,
            "every non-ignored file must produce one row"
        );
        assert_eq!(outcome.walk_errors, 0, "clean tree walks without errors");
    }

    // --- a missing root is a counted error, not a silent empty success ---

    #[test]
    fn missing_root_reports_walk_error_not_empty_success() {
        let dir = tempfile::tempdir().unwrap();
        let gone = dir.path().join("never-existed");
        let outcome = walk_repo("r", &gone);
        assert!(outcome.entries.is_empty());
        assert!(
            outcome.walk_errors > 0,
            "a vanished root must be visible to the caller so the prune \
             pass does not treat the empty set as the full corpus"
        );
    }

    // --- sh and md land with lang = None ---

    #[test]
    fn sh_and_md_have_no_lang() {
        let dir = make_tree(&["deploy.sh", "README.md"]);
        let entries = walk_repo("r", dir.path()).entries;
        assert_eq!(entries.len(), 2, "both files must be inventoried");
        for e in &entries {
            assert!(e.lang.is_none(), "{} should have lang=None", e.path);
        }
    }

    // --- docs-only repo (no language markers) succeeds ---

    #[test]
    fn docs_only_repo_produces_inventory() {
        let dir = make_tree(&["docs/guide.md", "docs/intro.md", "README.md"]);
        let entries = walk_repo("docs-repo", dir.path()).entries;
        // Must succeed and return a row for each file, not error.
        assert_eq!(entries.len(), 3);
        for e in &entries {
            assert!(
                e.lang.is_none(),
                "docs-only repo: lang must be None for {}",
                e.path
            );
            assert_eq!(e.ext.as_deref(), Some("md"));
        }
    }

    // --- paths are repo-relative ---

    #[test]
    fn walk_paths_are_repo_relative() {
        let dir = make_tree(&["src/lib.rs"]);
        let entries = walk_repo("r", dir.path()).entries;
        assert_eq!(entries.len(), 1);
        // Must be repo-relative, not absolute.
        assert!(
            !entries[0].path.starts_with('/'),
            "path must be repo-relative"
        );
        assert_eq!(entries[0].path, "src/lib.rs");
    }

    // --- dotfiles and dot-directories are included ---

    #[test]
    fn dotfiles_are_included() {
        // Files like .github/workflows/ci.yml and .env must appear in the
        // inventory; WalkBuilder defaults to hidden(true) which excludes them,
        // so we explicitly set hidden(false) in walk_repo.
        let dir = make_tree(&[".github/workflows/ci.yml", ".env.example", "src/main.rs"]);
        let entries = walk_repo("r", dir.path()).entries;
        let paths: Vec<&str> = entries.iter().map(|e| e.path.as_str()).collect();
        assert!(
            paths.contains(&".github/workflows/ci.yml"),
            ".github/workflows/ci.yml must be inventoried; got: {paths:?}"
        );
        assert!(
            paths.contains(&".env.example"),
            ".env.example must be inventoried; got: {paths:?}"
        );
        assert!(paths.contains(&"src/main.rs"));
        assert_eq!(entries.len(), 3, "all three files must appear");
    }

    // --- .git object store is never inventoried ---

    #[test]
    fn git_dir_is_excluded_even_with_hidden_false() {
        // walk_repo sets hidden(false) to include dotfiles; this must NOT
        // pull in the .git object store. Pins the doc claim on walk_repo
        // that the ignore crate skips .git regardless of hidden().
        let dir = make_tree(&[".git/config", ".git/objects/aa/bb", "src/main.rs"]);
        let entries = walk_repo("r", dir.path()).entries;
        let paths: Vec<&str> = entries.iter().map(|e| e.path.as_str()).collect();
        assert!(
            !paths.iter().any(|p| p.starts_with(".git/")),
            ".git/ contents must never be inventoried; got: {paths:?}"
        );
        assert!(paths.contains(&"src/main.rs"));
    }

    // --- gitignored files are excluded ---

    #[test]
    fn gitignored_files_are_excluded() {
        let dir = make_tree(&["src/main.rs", "target/debug/legion"]);
        // Write a .gitignore that excludes the target/ directory.
        fs::write(dir.path().join(".gitignore"), b"target/\n").unwrap();
        let entries = walk_repo("r", dir.path()).entries;
        let paths: Vec<&str> = entries.iter().map(|e| e.path.as_str()).collect();
        assert!(
            paths.contains(&"src/main.rs"),
            "src/main.rs should be present"
        );
        assert!(
            !paths.iter().any(|p| p.starts_with("target/")),
            "target/ should be gitignored"
        );
    }

    // --- walk output tracks tree mutations (add + delete visible) ---

    #[test]
    fn walk_reflects_tree_changes_between_runs() {
        let dir = make_tree(&["a.rs", "b.rs"]);
        let first = walk_repo("r", dir.path()).entries;
        assert_eq!(first.len(), 2);

        std::fs::remove_file(dir.path().join("b.rs")).unwrap();
        std::fs::write(dir.path().join("c.md"), b"new").unwrap();

        let second = walk_repo("r", dir.path()).entries;
        let mut second_paths: Vec<&str> = second.iter().map(|e| e.path.as_str()).collect();
        second_paths.sort_unstable();
        assert_eq!(
            second_paths,
            vec!["a.rs", "c.md"],
            "walk must reflect deletions and additions, not cache"
        );
    }

    // --- symlinks are excluded (walker does not follow links) ---

    #[cfg(unix)]
    #[test]
    fn symlinks_are_excluded_from_inventory() {
        let dir = make_tree(&["real.rs"]);
        std::os::unix::fs::symlink(dir.path().join("real.rs"), dir.path().join("link.rs")).unwrap();
        let entries = walk_repo("r", dir.path()).entries;
        let paths: Vec<&str> = entries.iter().map(|e| e.path.as_str()).collect();
        assert_eq!(
            paths,
            vec!["real.rs"],
            "symlink entries must be skipped (follow_links off + is_file guard)"
        );
    }
}
