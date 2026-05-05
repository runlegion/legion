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

/// Detect every language with a recognized marker file in `repo_path`.
/// Returns the language tags legion uses internally; empty when none of
/// the supported indexers' marker files are present.
///
/// Markers checked:
/// - `Cargo.toml` -> "rust"
/// - `package.json` -> "typescript"
/// - `pyproject.toml` or `requirements.txt` -> "python"
/// - `go.mod` -> "go"
///
/// A polyglot repo (e.g. Rust core + TS dashboard) returns multiple
/// languages and the caller indexes each independently into its own
/// `(repo, lang)` row.
pub fn detect_languages(repo_path: &Path) -> Vec<&'static str> {
    let mut langs: Vec<&'static str> = Vec::new();
    if repo_path.join("Cargo.toml").is_file() {
        langs.push("rust");
    }
    if repo_path.join("package.json").is_file() {
        langs.push("typescript");
    }
    if repo_path.join("pyproject.toml").is_file() || repo_path.join("requirements.txt").is_file() {
        langs.push("python");
    }
    if repo_path.join("go.mod").is_file() {
        langs.push("go");
    }
    langs
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
        "typescript" => run_scip_typescript(repo_path),
        "python" => run_scip_python(repo_path),
        "go" => run_scip_go(repo_path),
        other => Err(LegionError::IndexerNotFound {
            lang: other.to_string(),
            binary: format!("scip-{other}"),
        }),
    }
}

/// Invoke a Rust SCIP indexer against `repo_path` and read the resulting
/// `index.scip` protobuf bytes from the repo root.
///
/// Tries `scip-rust index` first (the original sourcegraph indexer), then
/// falls back to `rust-analyzer scip .` (#381). The legacy scip-rust repo
/// is archived and has no installable distribution on crates.io as of
/// 2026-04, so most fresh dev machines only have rust-analyzer (shipped
/// with rustup). Both produce the same SCIP protobuf shape.
fn run_scip_rust(repo_path: &Path) -> Result<Vec<u8>> {
    match run_indexer_binary("rust", "scip-rust", &["index"], repo_path) {
        Ok(bytes) => Ok(bytes),
        Err(LegionError::IndexerNotFound { .. }) => {
            run_indexer_binary("rust", "rust-analyzer", &["scip", "."], repo_path)
                .map_err(rust_install_hint)
        }
        Err(other) => Err(other),
    }
}

/// Map a missing-binary error from the rust-analyzer fallback to one that
/// names both candidate binaries plus the rustup install hint.
fn rust_install_hint(e: LegionError) -> LegionError {
    match e {
        LegionError::IndexerNotFound { lang, .. } => LegionError::IndexerNotFound {
            lang,
            binary: "scip-rust or rust-analyzer (install: rustup component add rust-analyzer)"
                .to_string(),
        },
        other => other,
    }
}

/// Invoke `scip-typescript index` against `repo_path`. Canonical TS/JS
/// indexer from sourcegraph (`npm i -g @sourcegraph/scip-typescript`).
/// No fallback exists; tsserver is too slow and shape-incompatible to
/// substitute.
fn run_scip_typescript(repo_path: &Path) -> Result<Vec<u8>> {
    run_indexer_binary("typescript", "scip-typescript", &["index"], repo_path).map_err(
        |e| match e {
            LegionError::IndexerNotFound { lang, .. } => LegionError::IndexerNotFound {
                lang,
                binary: "scip-typescript (install: npm i -g @sourcegraph/scip-typescript)"
                    .to_string(),
            },
            other => other,
        },
    )
}

/// Invoke `scip-python index .` against `repo_path`. Canonical Python
/// indexer from sourcegraph (`pip install scip-python` or
/// `npm i -g @sourcegraph/scip-python`).
fn run_scip_python(repo_path: &Path) -> Result<Vec<u8>> {
    run_indexer_binary("python", "scip-python", &["index", "."], repo_path).map_err(|e| match e {
        LegionError::IndexerNotFound { lang, .. } => LegionError::IndexerNotFound {
            lang,
            binary: "scip-python (install: pip install scip-python)".to_string(),
        },
        other => other,
    })
}

/// Invoke `scip-go` against `repo_path`. Canonical Go indexer from
/// sourcegraph (`go install github.com/sourcegraph/scip-go/cmd/scip-go@latest`).
/// Writes `index.scip` in the working directory by default; no subcommand.
fn run_scip_go(repo_path: &Path) -> Result<Vec<u8>> {
    run_indexer_binary("go", "scip-go", &[], repo_path).map_err(|e| match e {
        LegionError::IndexerNotFound { lang, .. } => LegionError::IndexerNotFound {
            lang,
            binary:
                "scip-go (install: go install github.com/sourcegraph/scip-go/cmd/scip-go@latest)"
                    .to_string(),
        },
        other => other,
    })
}

/// Run a SCIP indexer binary against `repo_path`. Returns the bytes of the
/// `index.scip` protobuf written into the repo root, or a typed error
/// describing the failure mode (binary not on PATH, subprocess exited
/// non-zero, output file unreadable). The `lang` is carried into error
/// variants so callers see which language failed even when several share
/// this helper.
fn run_indexer_binary(
    lang: &str,
    binary: &str,
    args: &[&str],
    repo_path: &Path,
) -> Result<Vec<u8>> {
    let output = match Command::new(binary)
        .args(args)
        .current_dir(repo_path)
        .output()
    {
        Ok(o) => o,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(LegionError::IndexerNotFound {
                lang: lang.to_string(),
                binary: binary.to_string(),
            });
        }
        Err(e) => return Err(LegionError::Io(e)),
    };

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        return Err(LegionError::IndexerFailed {
            lang: lang.to_string(),
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
    fn detect_languages_rust_for_cargo_toml() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("Cargo.toml"), "[package]\nname = \"x\"").unwrap();
        assert_eq!(detect_languages(dir.path()), vec!["rust"]);
    }

    #[test]
    fn detect_languages_typescript_for_package_json() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("package.json"), "{\"name\":\"x\"}").unwrap();
        assert_eq!(detect_languages(dir.path()), vec!["typescript"]);
    }

    #[test]
    fn detect_languages_python_for_either_marker() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("pyproject.toml"), "[project]\nname=\"x\"").unwrap();
        assert_eq!(detect_languages(dir.path()), vec!["python"]);

        let dir2 = TempDir::new().unwrap();
        fs::write(dir2.path().join("requirements.txt"), "requests").unwrap();
        assert_eq!(detect_languages(dir2.path()), vec!["python"]);
    }

    #[test]
    fn detect_languages_go_for_go_mod() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("go.mod"), "module x\n").unwrap();
        assert_eq!(detect_languages(dir.path()), vec!["go"]);
    }

    #[test]
    fn detect_languages_polyglot_returns_all() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("Cargo.toml"), "[package]\nname=\"x\"").unwrap();
        fs::write(dir.path().join("package.json"), "{\"name\":\"x\"}").unwrap();
        let langs = detect_languages(dir.path());
        assert!(langs.contains(&"rust"));
        assert!(langs.contains(&"typescript"));
        assert_eq!(langs.len(), 2);
    }

    #[test]
    fn detect_languages_empty_when_no_markers() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("README.md"), "hi").unwrap();
        assert!(detect_languages(dir.path()).is_empty());
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

    /// Verify each language helper surfaces its install hint when its
    /// indexer is missing from PATH. Using a single combined test with
    /// PATH manipulation to avoid the parallel cargo-test race.
    #[cfg(unix)]
    #[test]
    fn language_helpers_surface_install_hints_when_missing() {
        let prior = std::env::var("PATH").unwrap_or_default();
        let (empty_dir, empty_path) = isolate_path_with_shims(&[]);
        let repo = TempDir::new().unwrap();
        // Safety: see run_scip_rust_fallback_chain for the same justification.
        unsafe {
            std::env::set_var("PATH", &empty_path);
        }

        let cases = [
            (
                "typescript",
                run_scip_typescript(repo.path()),
                "scip-typescript",
                "npm",
            ),
            ("python", run_scip_python(repo.path()), "scip-python", "pip"),
            ("go", run_scip_go(repo.path()), "scip-go", "go install"),
        ];

        unsafe {
            std::env::set_var("PATH", &prior);
        }
        drop(empty_dir);

        for (lang, result, expected_binary, expected_install_hint) in cases {
            let err = result.unwrap_err();
            match err {
                LegionError::IndexerNotFound { lang: l, binary } => {
                    assert_eq!(l, lang);
                    assert!(
                        binary.contains(expected_binary),
                        "{lang} hint missing binary name: {binary}"
                    );
                    assert!(
                        binary.contains(expected_install_hint),
                        "{lang} hint missing install instruction: {binary}"
                    );
                }
                other => panic!("expected IndexerNotFound for {lang}, got {other:?}"),
            }
        }
    }

    /// Build a PATH that contains only the listed shim names (each shim a
    /// minimal bash script the test wrote into a tempdir). Returns the
    /// tempdir handle so the caller can keep it alive across the spawn.
    fn isolate_path_with_shims(shims: &[(&str, &str)]) -> (TempDir, String) {
        let dir = TempDir::new().unwrap();
        for (name, body) in shims {
            let path = dir.path().join(name);
            fs::write(&path, body).unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let mut perms = fs::metadata(&path).unwrap().permissions();
                perms.set_mode(0o755);
                fs::set_permissions(&path, perms).unwrap();
            }
        }
        let path_str = dir.path().to_string_lossy().into_owned();
        (dir, path_str)
    }

    /// Two cargo tests must not concurrently mutate $PATH. Combining the
    /// fallback-success and both-missing assertions into a single test
    /// avoids the parallel-test race without pulling in a serial-test
    /// dependency.
    #[cfg(unix)]
    #[test]
    fn run_scip_rust_fallback_chain() {
        let prior = std::env::var("PATH").unwrap_or_default();

        // Phase 1: rust-analyzer shim available -> fallback succeeds.
        let body = "#!/bin/bash\nprintf 'fake-scip-bytes' > index.scip\nexit 0\n";
        let (shim_dir, shim_path) = isolate_path_with_shims(&[("rust-analyzer", body)]);
        let repo = TempDir::new().unwrap();
        // Safety: cargo test default parallelism allows other tests in
        // this binary to run concurrently. Other tests in this module do
        // not invoke run_scip_rust or shell out via PATH, and we restore
        // the prior PATH before exiting either phase.
        unsafe {
            std::env::set_var("PATH", &shim_path);
        }
        let bytes_result = run_scip_rust(repo.path());
        unsafe {
            std::env::set_var("PATH", &prior);
        }
        let bytes = bytes_result.expect("rust-analyzer fallback should succeed");
        assert_eq!(bytes, b"fake-scip-bytes");
        drop(shim_dir);

        // Phase 2: empty PATH (no binaries) -> IndexerNotFound naming both.
        let (empty_dir, empty_path) = isolate_path_with_shims(&[]);
        let repo2 = TempDir::new().unwrap();
        unsafe {
            std::env::set_var("PATH", &empty_path);
        }
        let err = run_scip_rust(repo2.path()).unwrap_err();
        unsafe {
            std::env::set_var("PATH", &prior);
        }
        drop(empty_dir);
        match err {
            LegionError::IndexerNotFound { lang, binary } => {
                assert_eq!(lang, "rust");
                assert!(
                    binary.contains("scip-rust") && binary.contains("rust-analyzer"),
                    "error must name both binaries; got: {binary}"
                );
            }
            other => panic!("expected IndexerNotFound, got {other:?}"),
        }
    }
}
