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
use sha2::{Digest, Sha256};
use std::path::Path;
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

/// SHA-256 of `bytes` rendered as lowercase hex.
pub fn content_hash(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let digest = hasher.finalize();
    hex::encode(digest)
}

/// Detect the dominant language of a repo by looking for marker files.
/// Returns the language tag legion uses internally (e.g. "rust"), or
/// None when no indexer-supported language is recognized.
///
/// This issue (#278) only handles Rust. Multi-language detection lives
/// in #279; this function exists here so the dispatch path is already
/// shaped correctly.
pub fn detect_language(repo_path: &Path) -> Option<&'static str> {
    if repo_path.join("Cargo.toml").is_file() {
        return Some("rust");
    }
    None
}

/// Run the appropriate SCIP indexer for `lang` against `repo_path` and
/// return the resulting protobuf bytes.
///
/// Errors:
/// - `IndexerNotFound` when the binary is missing from PATH
/// - `IndexerFailed` when the subprocess exits non-zero (carries stderr)
/// - `Io` when the protobuf output file cannot be read
pub fn run_indexer(lang: &str, repo_path: &Path) -> Result<Vec<u8>> {
    match lang {
        "rust" => run_scip_rust(repo_path),
        other => Err(LegionError::IndexerNotFound {
            lang: other.to_string(),
            binary: format!("scip-{other}"),
        }),
    }
}

/// Invoke `scip-rust index` against `repo_path` and read the resulting
/// `index.scip` protobuf bytes from the repo root.
fn run_scip_rust(repo_path: &Path) -> Result<Vec<u8>> {
    let binary = "scip-rust";
    let output = Command::new(binary)
        .arg("index")
        .current_dir(repo_path)
        .output();

    let output = match output {
        Ok(o) => o,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(LegionError::IndexerNotFound {
                lang: "rust".to_string(),
                binary: binary.to_string(),
            });
        }
        Err(e) => return Err(LegionError::Io(e)),
    };

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        return Err(LegionError::IndexerFailed {
            lang: "rust".to_string(),
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

    #[test]
    fn content_hash_is_deterministic_and_distinct() {
        let a = content_hash(b"hello");
        let b = content_hash(b"hello");
        let c = content_hash(b"world");
        assert_eq!(a, b);
        assert_ne!(a, c);
        assert_eq!(a.len(), 64); // 32 bytes -> 64 hex chars
    }

    #[test]
    fn detect_language_rust_for_cargo_toml() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("Cargo.toml"), "[package]\nname = \"x\"").unwrap();
        assert_eq!(detect_language(dir.path()), Some("rust"));
    }

    #[test]
    fn detect_language_none_for_unsupported() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("README.md"), "hi").unwrap();
        assert_eq!(detect_language(dir.path()), None);
    }

    #[test]
    fn run_indexer_unsupported_language_errors() {
        let dir = TempDir::new().unwrap();
        let err = run_indexer("klingon", dir.path()).unwrap_err();
        match err {
            LegionError::IndexerNotFound { lang, binary } => {
                assert_eq!(lang, "klingon");
                assert_eq!(binary, "scip-klingon");
            }
            other => panic!("expected IndexerNotFound, got {other:?}"),
        }
    }
}
