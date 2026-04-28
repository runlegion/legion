//! SCIP (Source Code Intelligence Protocol) indexing for legion.
//!
//! This module is the storage and dispatch layer for SCIP protobuf blobs.
//! Per repo, per language, legion holds one active blob in the
//! `scip_indexes` table; query operations on top of those blobs land in
//! later issues (#282 query CLI, #285 cross-repo).
//!
//! The protobuf bytes themselves are never parsed at write time -- the
//! blob is opaque to legion until a query path needs it. We do compute
//! a SHA-256 over the bytes so an upsert with unchanged content can
//! short-circuit to bumping `updated_at` without rewriting the BLOB
//! column (see `Database::upsert_scip_index`).

use crate::error::{LegionError, Result};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::process::Command;

/// A stored SCIP index for one (repo, lang) pair.
#[derive(Debug, Clone)]
pub struct ScipIndex {
    pub id: String,
    pub repo: String,
    pub lang: String,
    pub content_hash: String,
    pub blob: Vec<u8>,
    pub updated_at: String,
    /// Soft-delete tombstone for smugglr sync. Populated by future
    /// cleanup paths (#284 daemon indexer); not read by the #278 base.
    #[allow(dead_code)]
    pub deleted_at: Option<String>,
}

/// A language detected in a repo and the binary used to index it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DetectedLanguage {
    pub lang: String,
    pub binary: String,
}

/// Result of scanning a repo for indexable languages.
#[derive(Debug, Default)]
pub struct DetectionResult {
    /// Languages whose `scip-<lang>` binary is on PATH and ready to run.
    pub indexable: Vec<DetectedLanguage>,
    /// Languages we recognized in the source tree but whose indexer
    /// binary was not found on PATH. Surfaced as warnings, not errors.
    pub missing_binary: Vec<DetectedLanguage>,
}

/// Per-repo index configuration. Loaded from the per-repo `extra` table
/// in `watch.toml`; the field is optional so an empty config falls back
/// to the built-in language table.
#[derive(Debug, Default, Deserialize)]
pub struct IndexConfig {
    /// Override or extend the built-in lang -> binary map. A key here
    /// supersedes the default for the same language. Keys outside the
    /// detection table are ignored (they have no extension to match on).
    pub indexers: Option<HashMap<String, String>>,
}

/// Built-in language table: extension markers and the default binary.
/// Sorted alphabetically by language name so iteration order is stable
/// across runs and `detect_languages` returns indexable + missing in
/// the same canonical order.
struct LangSpec {
    lang: &'static str,
    extensions: &'static [&'static str],
    default_binary: &'static str,
}

const LANG_TABLE: &[LangSpec] = &[
    LangSpec {
        lang: "go",
        extensions: &["go"],
        default_binary: "scip-go",
    },
    LangSpec {
        lang: "javascript",
        extensions: &["js", "jsx"],
        default_binary: "scip-typescript",
    },
    LangSpec {
        lang: "python",
        extensions: &["py"],
        default_binary: "scip-python",
    },
    LangSpec {
        lang: "rust",
        extensions: &["rs"],
        default_binary: "scip-rust",
    },
    LangSpec {
        lang: "typescript",
        extensions: &["ts", "tsx"],
        default_binary: "scip-typescript",
    },
];

/// Directory names a shallow scan should never descend into. Indexers
/// emit their own SCIP output here in some configurations and we don't
/// want vendored or build-artifact languages confusing detection.
const SKIP_DIRS: &[&str] = &[
    ".git",
    "node_modules",
    "target",
    "vendor",
    "dist",
    "build",
    "__pycache__",
];

const SCAN_DEPTH: usize = 2;

/// Resolve the language for a single file path by extension. Returns
/// the canonical legion lang tag (e.g. "rust"). The built-in extension
/// table is the single source of truth -- `IndexConfig.indexers`
/// overrides binaries, not extension recognition.
pub fn lang_for_path(path: &Path) -> Option<String> {
    let ext = path.extension()?.to_str()?.to_lowercase();
    for spec in LANG_TABLE {
        if spec.extensions.iter().any(|e| *e == ext) {
            return Some(spec.lang.to_string());
        }
    }
    None
}

/// Resolve the binary used for a given language under `config`.
fn binary_for_lang(lang: &str, config: &IndexConfig) -> Option<String> {
    if let Some(map) = config.indexers.as_ref()
        && let Some(b) = map.get(lang)
    {
        return Some(b.clone());
    }
    LANG_TABLE
        .iter()
        .find(|s| s.lang == lang)
        .map(|s| s.default_binary.to_string())
}

/// Re-index the language for `file_path` and store the resulting blob
/// against (repo, lang). When the language indexer does not support
/// per-file mode, falls back to a single-language full repo index --
/// the fallback emits a stderr warning so the operator knows the
/// command did more work than the name suggests.
///
/// Returns the new content_hash on success.
pub fn incremental_index_file(
    db: &crate::db::Database,
    repo: &str,
    repo_path: &Path,
    file_path: &Path,
    config: &IndexConfig,
) -> Result<String> {
    incremental_index_file_with(db, repo, repo_path, file_path, config, run_indexer)
}

/// Internal entry point that lets tests inject a stub indexer instead
/// of shelling out to `scip-<lang>`. Production callers go through
/// [`incremental_index_file`].
pub(crate) fn incremental_index_file_with<F>(
    db: &crate::db::Database,
    repo: &str,
    repo_path: &Path,
    file_path: &Path,
    config: &IndexConfig,
    runner: F,
) -> Result<String>
where
    F: Fn(&DetectedLanguage, &Path) -> Result<Vec<u8>>,
{
    if !file_path.exists() {
        return Err(LegionError::Io(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("file not found: {}", file_path.display()),
        )));
    }

    let ext_for_msg = file_path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("(none)")
        .to_string();

    let lang = lang_for_path(file_path).ok_or_else(|| LegionError::IndexerNotFound {
        lang: ext_for_msg.clone(),
        binary: "(unknown)".to_string(),
    })?;

    let binary = binary_for_lang(&lang, config).ok_or_else(|| LegionError::IndexerNotFound {
        lang: lang.clone(),
        binary: "(unknown)".to_string(),
    })?;

    // No scip-<lang> tool currently exposes a per-file mode legion can
    // rely on -- spec acknowledges this by mandating a fallback path.
    // Always log the fallback so operators understand the cost.
    eprintln!(
        "[legion index] fallback to full single-language re-index for {lang} (file: {})",
        file_path.display()
    );

    let entry = DetectedLanguage {
        lang: lang.clone(),
        binary,
    };
    let blob = runner(&entry, repo_path)?;
    let hash = content_hash(&blob);

    let index = ScipIndex {
        id: uuid::Uuid::now_v7().to_string(),
        repo: repo.to_string(),
        lang,
        content_hash: hash.clone(),
        blob,
        updated_at: chrono::Utc::now().to_rfc3339(),
        deleted_at: None,
    };
    db.upsert_scip_index(&index)?;
    Ok(hash)
}

/// SHA-256 of `bytes` rendered as lowercase hex.
pub fn content_hash(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let digest = hasher.finalize();
    hex::encode(digest)
}

/// Scan `repo_root` for source files matching the built-in extension
/// table and return languages partitioned into indexable (binary on
/// PATH) and missing (binary absent). Order within each list is
/// alphabetical by language name.
///
/// `config.indexers` overrides the default binary per language. Keys
/// outside `LANG_TABLE` are ignored -- without an extension marker we
/// have no way to detect that language anyway.
pub fn detect_languages(repo_root: &Path, config: &IndexConfig) -> DetectionResult {
    let extensions_seen = scan_extensions(repo_root);
    let overrides = config.indexers.as_ref();

    let mut result = DetectionResult::default();
    for spec in LANG_TABLE {
        if !spec.extensions.iter().any(|e| extensions_seen.contains(*e)) {
            continue;
        }
        let binary = overrides
            .and_then(|m| m.get(spec.lang))
            .map(String::as_str)
            .unwrap_or(spec.default_binary)
            .to_string();
        let entry = DetectedLanguage {
            lang: spec.lang.to_string(),
            binary,
        };
        if binary_on_path(&entry.binary) {
            result.indexable.push(entry);
        } else {
            result.missing_binary.push(entry);
        }
    }
    result
}

/// Return true when an executable named `name` exists on PATH.
fn binary_on_path(name: &str) -> bool {
    let Some(path) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&path).any(|dir| {
        let candidate = dir.join(name);
        candidate.is_file()
    })
}

/// Walk the repo root up to `SCAN_DEPTH` levels deep and collect every
/// file extension seen, skipping vendored/build directories. Returns
/// extensions without leading dots.
fn scan_extensions(root: &Path) -> BTreeSet<String> {
    let mut found: BTreeSet<String> = BTreeSet::new();
    let mut stack: Vec<(PathBuf, usize)> = vec![(root.to_path_buf(), 0)];
    while let Some((dir, depth)) = stack.pop() {
        let entries = match std::fs::read_dir(&dir) {
            Ok(e) => e,
            Err(_) => continue,
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name();
            let name_str = name.to_string_lossy();
            let file_type = match entry.file_type() {
                Ok(t) => t,
                Err(_) => continue,
            };
            if file_type.is_dir() {
                if SKIP_DIRS.iter().any(|s| *s == name_str) {
                    continue;
                }
                if depth + 1 < SCAN_DEPTH {
                    stack.push((path, depth + 1));
                }
                continue;
            }
            if !file_type.is_file() {
                continue;
            }
            if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
                found.insert(ext.to_lowercase());
            }
        }
    }
    found
}

/// Run the SCIP indexer named by `entry.binary` against `repo_path` and
/// return the resulting protobuf bytes.
pub fn run_indexer(entry: &DetectedLanguage, repo_path: &Path) -> Result<Vec<u8>> {
    let output = Command::new(&entry.binary)
        .arg("index")
        .current_dir(repo_path)
        .output();

    let output = match output {
        Ok(o) => o,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(LegionError::IndexerNotFound {
                lang: entry.lang.clone(),
                binary: entry.binary.clone(),
            });
        }
        Err(e) => return Err(LegionError::Io(e)),
    };

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        return Err(LegionError::IndexerFailed {
            lang: entry.lang.clone(),
            stderr,
        });
    }

    let scip_path = repo_path.join("index.scip");
    let bytes = std::fs::read(&scip_path)?;
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn touch(dir: &Path, name: &str) {
        fs::write(dir.join(name), b"").unwrap();
    }

    #[test]
    fn content_hash_is_deterministic_and_distinct() {
        let a = content_hash(b"hello");
        let b = content_hash(b"hello");
        let c = content_hash(b"world");
        assert_eq!(a, b);
        assert_ne!(a, c);
        assert_eq!(a.len(), 64);
    }

    #[test]
    fn detect_languages_rust_only_for_rs_files() {
        let dir = TempDir::new().unwrap();
        touch(dir.path(), "main.rs");
        let combined = detect_languages(dir.path(), &IndexConfig::default());
        let langs: Vec<&str> = combined
            .indexable
            .iter()
            .chain(combined.missing_binary.iter())
            .map(|d| d.lang.as_str())
            .collect();
        assert_eq!(langs, vec!["rust"]);
    }

    #[test]
    fn detect_languages_returns_alphabetical_order_for_mixed_repo() {
        let dir = TempDir::new().unwrap();
        touch(dir.path(), "main.rs");
        touch(dir.path(), "script.py");
        let combined = detect_languages(dir.path(), &IndexConfig::default());
        let langs: Vec<&str> = combined
            .indexable
            .iter()
            .chain(combined.missing_binary.iter())
            .map(|d| d.lang.as_str())
            .collect();
        // Python sorts before Rust alphabetically -- the table is
        // iterated in canonical order regardless of which list each
        // lang lands in.
        let mut sorted = langs.clone();
        sorted.sort();
        assert_eq!(langs, sorted);
        assert!(langs.contains(&"python"));
        assert!(langs.contains(&"rust"));
    }

    // Test for partition-on-PATH-presence omitted: it would require
    // mutating $PATH, which races with other tests under the parallel
    // harness. The partitioning logic is exercised end-to-end whenever
    // `legion index` runs against a repo whose binary is/is-not on PATH.

    #[test]
    fn detect_languages_skips_vendor_dirs() {
        let dir = TempDir::new().unwrap();
        fs::create_dir(dir.path().join("node_modules")).unwrap();
        touch(&dir.path().join("node_modules"), "noise.rs");
        let result = detect_languages(dir.path(), &IndexConfig::default());
        assert!(result.indexable.is_empty() && result.missing_binary.is_empty());
    }

    #[test]
    fn detect_languages_honors_indexer_override() {
        let dir = TempDir::new().unwrap();
        touch(dir.path(), "main.rs");
        let mut overrides = HashMap::new();
        overrides.insert("rust".to_string(), "scip-custom".to_string());
        let cfg = IndexConfig {
            indexers: Some(overrides),
        };
        let result = detect_languages(dir.path(), &cfg);
        let entry = result
            .indexable
            .iter()
            .chain(result.missing_binary.iter())
            .find(|d| d.lang == "rust")
            .unwrap();
        assert_eq!(entry.binary, "scip-custom");
    }

    #[test]
    fn run_indexer_missing_binary_returns_indexer_not_found() {
        let dir = TempDir::new().unwrap();
        let entry = DetectedLanguage {
            lang: "klingon".to_string(),
            binary: "definitely-not-on-path-xyz".to_string(),
        };
        let err = run_indexer(&entry, dir.path()).unwrap_err();
        match err {
            LegionError::IndexerNotFound { lang, binary } => {
                assert_eq!(lang, "klingon");
                assert_eq!(binary, "definitely-not-on-path-xyz");
            }
            other => panic!("expected IndexerNotFound, got {other:?}"),
        }
    }

    #[test]
    fn lang_for_path_recognises_known_extensions() {
        assert_eq!(
            lang_for_path(Path::new("src/main.rs")).as_deref(),
            Some("rust")
        );
        assert_eq!(
            lang_for_path(Path::new("app/handler.ts")).as_deref(),
            Some("typescript")
        );
        assert_eq!(
            lang_for_path(Path::new("scripts/run.py")).as_deref(),
            Some("python")
        );
    }

    #[test]
    fn lang_for_path_returns_none_for_unknown() {
        assert!(lang_for_path(Path::new("README.md")).is_none());
        assert!(lang_for_path(Path::new("data.bin.unknown")).is_none());
    }

    fn open_test_db() -> crate::db::Database {
        let dir = TempDir::new().unwrap();
        let db = crate::db::Database::open(&dir.path().join("test.sqlite")).unwrap();
        // TempDir would be dropped at end of expression; leak it so the
        // sqlite file outlives the test.
        std::mem::forget(dir);
        db
    }

    #[test]
    fn incremental_index_file_creates_row_then_updates_in_place() {
        let db = open_test_db();
        let repo_dir = TempDir::new().unwrap();
        let file_path = repo_dir.path().join("main.rs");
        fs::write(&file_path, "fn main() {}").unwrap();
        let cfg = IndexConfig::default();

        // First call: stub runner returns "v1" -- expect a fresh row.
        let hash1 = incremental_index_file_with(
            &db,
            "testrepo",
            repo_dir.path(),
            &file_path,
            &cfg,
            |_, _| Ok(b"v1".to_vec()),
        )
        .unwrap();
        let row1 = db.get_scip_index("testrepo", "rust").unwrap().unwrap();
        assert_eq!(row1.content_hash, hash1);
        assert_eq!(row1.blob, b"v1");

        // Second call with identical bytes: hash unchanged, row id stable.
        let hash2 = incremental_index_file_with(
            &db,
            "testrepo",
            repo_dir.path(),
            &file_path,
            &cfg,
            |_, _| Ok(b"v1".to_vec()),
        )
        .unwrap();
        assert_eq!(hash1, hash2);
        let row2 = db.get_scip_index("testrepo", "rust").unwrap().unwrap();
        assert_eq!(row2.id, row1.id);

        // Third call with different bytes: hash changes, row updates.
        let hash3 = incremental_index_file_with(
            &db,
            "testrepo",
            repo_dir.path(),
            &file_path,
            &cfg,
            |_, _| Ok(b"v2".to_vec()),
        )
        .unwrap();
        assert_ne!(hash3, hash1);
        let row3 = db.get_scip_index("testrepo", "rust").unwrap().unwrap();
        assert_eq!(row3.blob, b"v2");
    }

    #[test]
    fn incremental_index_file_unknown_extension_errors_with_unknown_binary() {
        let db = open_test_db();
        let repo_dir = TempDir::new().unwrap();
        let file_path = repo_dir.path().join("notes.txt");
        fs::write(&file_path, "hello").unwrap();
        let cfg = IndexConfig::default();
        let err = incremental_index_file(&db, "r", repo_dir.path(), &file_path, &cfg).unwrap_err();
        match err {
            LegionError::IndexerNotFound { lang, binary } => {
                assert_eq!(lang, "txt");
                assert_eq!(binary, "(unknown)");
            }
            other => panic!("expected IndexerNotFound, got {other:?}"),
        }
    }

    #[test]
    fn incremental_index_file_missing_file_returns_io_not_found() {
        let db = open_test_db();
        let repo_dir = TempDir::new().unwrap();
        let cfg = IndexConfig::default();
        let err = incremental_index_file(
            &db,
            "r",
            repo_dir.path(),
            &repo_dir.path().join("ghost.rs"),
            &cfg,
        )
        .unwrap_err();
        match err {
            LegionError::Io(e) => {
                assert_eq!(e.kind(), std::io::ErrorKind::NotFound);
            }
            other => panic!("expected Io NotFound, got {other:?}"),
        }
    }
}
