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
/// - `pom.xml`, `build.gradle`, or `build.gradle.kts` -> "java"
/// - `Gemfile` -> "ruby"
/// - `CMakeLists.txt` or `compile_commands.json` -> "clang" (C/C++)
/// - any `*.csproj` or `*.sln` in the repo root -> "csharp"
/// - `composer.json` -> "php"
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
    if repo_path.join("pom.xml").is_file()
        || repo_path.join("build.gradle").is_file()
        || repo_path.join("build.gradle.kts").is_file()
    {
        langs.push("java");
    }
    if repo_path.join("Gemfile").is_file() {
        langs.push("ruby");
    }
    if repo_path.join("CMakeLists.txt").is_file()
        || repo_path.join("compile_commands.json").is_file()
    {
        langs.push("clang");
    }
    if has_dotnet_project(repo_path) {
        langs.push("csharp");
    }
    if repo_path.join("composer.json").is_file() {
        langs.push("php");
    }
    langs
}

/// True when the repo root contains any `*.csproj` or `*.sln` file. Unlike
/// the other languages whose marker is a fixed filename, .NET projects
/// pick arbitrary names for the project file -- the convention is the
/// extension, not the basename. We scan the immediate root only; nested
/// projects under subdirectories are out of scope for the v1 marker.
fn has_dotnet_project(repo_path: &Path) -> bool {
    let Ok(entries) = std::fs::read_dir(repo_path) else {
        return false;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        match path.extension().and_then(|e| e.to_str()) {
            Some("csproj") | Some("sln") => return true,
            _ => {}
        }
    }
    false
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
        "java" => run_scip_java(repo_path),
        "ruby" => run_scip_ruby(repo_path),
        "clang" => run_scip_clang(repo_path),
        "csharp" => run_scip_dotnet(repo_path),
        "php" => run_scip_php(repo_path),
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
            run_indexer_binary("rust", "rust-analyzer", &["scip", "."], repo_path).map_err(
                with_install_hint(
                    "scip-rust or rust-analyzer (install: rustup component add rust-analyzer)",
                ),
            )
        }
        Err(other) => Err(other),
    }
}

/// Build a `map_err` closure that rewrites a missing-binary error's `binary`
/// field to a per-language install hint, leaving every other error variant
/// untouched. Shared by all the per-language indexer wrappers so the hint
/// text is the only thing that varies between them.
fn with_install_hint(hint: &'static str) -> impl FnOnce(LegionError) -> LegionError {
    move |e| match e {
        LegionError::IndexerNotFound { lang, .. } => LegionError::IndexerNotFound {
            lang,
            binary: hint.to_string(),
        },
        other => other,
    }
}

/// TypeScript workspace flavor detected at the repo root. Drives the
/// `--pnpm-workspaces` / `--yarn-workspaces` flag that scip-typescript
/// needs to walk a monorepo instead of honoring only the root tsconfig's
/// narrow `include`.
#[derive(Debug, PartialEq, Eq)]
enum TsWorkspaceFlavor {
    Pnpm,
    Yarn,
    None,
}

/// Inspect `repo_path` root for a workspace marker.
///
/// Precedence:
/// - `pnpm-workspace.yaml` present -> Pnpm (matches pnpm's own resolution
///   order: if the file exists, pnpm treats the repo as a pnpm workspace
///   regardless of what `package.json` says)
/// - `package.json` parses and contains a `workspaces` field (array or
///   object form, both yarn and npm shape) -> Yarn
/// - otherwise -> None
///
/// Without one of the workspace flags, scip-typescript honors only the
/// root tsconfig's `include` set. On a typical monorepo with a narrow
/// root tsconfig that produces an effectively empty index (rafters root
/// `include: ["test/**/*"]` -> 51KB blob with no `packages/*` symbols;
/// `scip-typescript index --pnpm-workspaces` from the same root -> 18MB
/// covering 13 workspaces). See #441.
fn detect_ts_workspace_flavor(repo_path: &Path) -> TsWorkspaceFlavor {
    if repo_path.join("pnpm-workspace.yaml").is_file() {
        return TsWorkspaceFlavor::Pnpm;
    }
    let pkg_json = repo_path.join("package.json");
    if let Ok(bytes) = std::fs::read(&pkg_json)
        && let Ok(value) = serde_json::from_slice::<serde_json::Value>(&bytes)
        && value.get("workspaces").is_some()
    {
        return TsWorkspaceFlavor::Yarn;
    }
    TsWorkspaceFlavor::None
}

/// Map a workspace flavor to the scip-typescript argv. Split out so a unit
/// test can pin the flag names: a silent rename in scip-typescript (or a
/// typo introduced here) would otherwise only surface as another empty
/// index in production. The flags are part of scip-typescript's public CLI
/// contract (`scip-typescript index --help`).
fn scip_typescript_args(flavor: TsWorkspaceFlavor) -> &'static [&'static str] {
    match flavor {
        TsWorkspaceFlavor::Pnpm => &["index", "--pnpm-workspaces"],
        TsWorkspaceFlavor::Yarn => &["index", "--yarn-workspaces"],
        TsWorkspaceFlavor::None => &["index"],
    }
}

/// Invoke `scip-typescript index` against `repo_path`. Canonical TS/JS
/// indexer from sourcegraph (`npm i -g @sourcegraph/scip-typescript`).
/// No fallback exists; tsserver is too slow and shape-incompatible to
/// substitute.
fn run_scip_typescript(repo_path: &Path) -> Result<Vec<u8>> {
    let args = scip_typescript_args(detect_ts_workspace_flavor(repo_path));
    run_indexer_binary("typescript", "scip-typescript", args, repo_path).map_err(with_install_hint(
        "scip-typescript (install: npm i -g @sourcegraph/scip-typescript)",
    ))
}

/// Invoke `scip-python index .` against `repo_path`. Canonical Python
/// indexer from sourcegraph (`pip install scip-python` or
/// `npm i -g @sourcegraph/scip-python`).
fn run_scip_python(repo_path: &Path) -> Result<Vec<u8>> {
    run_indexer_binary("python", "scip-python", &["index", "."], repo_path).map_err(
        with_install_hint("scip-python (install: pip install scip-python)"),
    )
}

/// Invoke `scip-java index` against `repo_path`. Canonical Java/Kotlin/
/// Scala indexer from sourcegraph (https://github.com/sourcegraph/scip-java).
/// Requires the project to have been built first (`mvn compile` or
/// `gradle build`) so target/build/ artifacts exist for scip-java to walk.
/// Subprocess stderr surfaces that requirement when the build is absent.
fn run_scip_java(repo_path: &Path) -> Result<Vec<u8>> {
    run_indexer_binary("java", "scip-java", &["index"], repo_path).map_err(with_install_hint(
        "scip-java (install: see https://github.com/sourcegraph/scip-java)",
    ))
}

/// Invoke `scip-ruby` against `repo_path`. Canonical Ruby indexer from
/// sourcegraph (`gem install scip-ruby`). Writes the index to the path
/// passed via `--index-file`.
fn run_scip_ruby(repo_path: &Path) -> Result<Vec<u8>> {
    run_indexer_binary(
        "ruby",
        "scip-ruby",
        &["--index-file", "index.scip", "."],
        repo_path,
    )
    .map_err(with_install_hint(
        "scip-ruby (install: gem install scip-ruby)",
    ))
}

/// Invoke `scip-clang` against `repo_path`. Canonical C/C++ indexer from
/// sourcegraph; distributed as a release binary, not a package manager
/// install. Requires `compile_commands.json` -- generate via
/// `cmake -DCMAKE_EXPORT_COMPILE_COMMANDS=ON` or `bear -- make`.
/// scip-clang's own stderr names the missing prereq when it can't find the
/// compdb file, so we let that surface unmodified.
fn run_scip_clang(repo_path: &Path) -> Result<Vec<u8>> {
    run_indexer_binary(
        "clang",
        "scip-clang",
        &["--compdb-path", "compile_commands.json"],
        repo_path,
    )
    .map_err(with_install_hint(
        "scip-clang (install: download release from https://github.com/sourcegraph/scip-clang/releases)",
    ))
}

/// Invoke `scip-dotnet index` against `repo_path`. Canonical .NET indexer
/// from sourcegraph (`dotnet tool install -g sourcegraph.scip.dotnet`).
/// scip-dotnet drives `dotnet build` internally; failure to find the
/// .NET SDK surfaces in scip-dotnet's own stderr.
fn run_scip_dotnet(repo_path: &Path) -> Result<Vec<u8>> {
    run_indexer_binary("csharp", "scip-dotnet", &["index"], repo_path).map_err(with_install_hint(
        "scip-dotnet (install: dotnet tool install -g sourcegraph.scip.dotnet)",
    ))
}

/// Invoke `scip-php` against `repo_path`. Sourcegraph PHP indexer
/// (`composer global require sourcegraph/scip-php`). The least-canonical
/// of the ecosystem indexers; if upstream is no longer maintained, the
/// caller should close #430 with that observation rather than landing a
/// half-working integration.
fn run_scip_php(repo_path: &Path) -> Result<Vec<u8>> {
    run_indexer_binary("php", "scip-php", &[], repo_path).map_err(with_install_hint(
        "scip-php (install: composer global require sourcegraph/scip-php)",
    ))
}

/// Invoke `scip-go` against `repo_path`. Canonical Go indexer from
/// sourcegraph (`go install github.com/sourcegraph/scip-go/cmd/scip-go@latest`).
/// Writes `index.scip` in the working directory by default; no subcommand.
fn run_scip_go(repo_path: &Path) -> Result<Vec<u8>> {
    run_indexer_binary("go", "scip-go", &[], repo_path).map_err(with_install_hint(
        "scip-go (install: go install github.com/sourcegraph/scip-go/cmd/scip-go@latest)",
    ))
}

/// Run a SCIP indexer binary against `repo_path`. Returns the bytes of the
/// `index.scip` protobuf the binary writes into the repo root, or a typed
/// error describing the failure mode (binary not on PATH, subprocess exited
/// non-zero, output file unreadable). The `lang` is carried into error
/// variants so callers see which language failed even when several share
/// this helper. The on-disk `index.scip` is removed once its bytes are read
/// (best-effort) -- it is a transient artifact, never read again after ingest.
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
    // The protobuf is transient: the caller ingests these bytes into the
    // SQLite scip_indexes column, and no regen ever reads the prior file as
    // input -- each run regenerates it from source. Left in the repo root it
    // reads as a stray artifact that agents repeatedly try to delete, so
    // remove it once the bytes are captured. Best-effort by design: the bytes
    // are already in hand, so a failed removal must not fail the index. The
    // `/index.scip` gitignore entry remains as a safety net for that edge.
    let _ = std::fs::remove_file(&scip_path);
    Ok(bytes)
}

/// Directory holding background-indexer log files. Rooted under
/// `XDG_STATE_HOME` (default `~/.local/state/`) so logs survive a reboot --
/// the previous `/tmp` location was wiped by the OS, which made debugging an
/// overnight index that died at 3am impossible. The directory is created on
/// demand by callers; this function only resolves the path.
pub fn index_log_dir() -> PathBuf {
    if let Ok(state) = std::env::var("XDG_STATE_HOME")
        && !state.is_empty()
    {
        return PathBuf::from(state).join("legion").join("index-logs");
    }
    if let Ok(home) = std::env::var("HOME")
        && !home.is_empty()
    {
        return PathBuf::from(home).join(".local/state/legion/index-logs");
    }
    // Stripped container or similar: neither XDG_STATE_HOME nor HOME is
    // set. Fall back to the system temp dir so logging still works, but
    // surface a warning -- this path will not survive a reboot, defeating
    // the migration's purpose. Operators should set HOME or
    // XDG_STATE_HOME explicitly in this environment.
    let fallback = std::env::temp_dir().join("legion-index-logs");
    eprintln!(
        "[legion] WARNING: neither XDG_STATE_HOME nor HOME set; index logs at {} will not survive reboot",
        fallback.display()
    );
    fallback
}

/// Log file path for a single repo's background indexer output. Caller
/// must ensure the parent directory exists before opening for write.
pub fn index_log_path(repo_name: &str) -> PathBuf {
    index_log_dir().join(format!("{repo_name}.log"))
}

/// Move any leftover `legion-index-*.log` files from `legacy_temp_dir`
/// into `new_dir`. Best-effort: per-file failures are silent (file may
/// be in use, may not be ours, may have permissions we don't own).
/// Idempotent -- safe to call repeatedly. Pure on its arguments so tests
/// can exercise the migration without mutating process env.
pub fn migrate_legacy_index_logs_in(new_dir: &Path, legacy_temp_dir: &Path) {
    if std::fs::create_dir_all(new_dir).is_err() {
        return;
    }
    let Ok(entries) = std::fs::read_dir(legacy_temp_dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if !name.starts_with("legion-index-") || !name.ends_with(".log") {
            continue;
        }
        let repo_name = name
            .trim_start_matches("legion-index-")
            .trim_end_matches(".log");
        if repo_name.is_empty() {
            continue;
        }
        let dest = new_dir.join(format!("{repo_name}.log"));
        let _ = std::fs::rename(&path, &dest);
    }
}

/// Production wrapper: migrate from `std::env::temp_dir()` into the
/// XDG_STATE_HOME-rooted location. Called from `spawn_background_indexer`
/// so the migration runs the first time any node touches the new code path.
pub fn migrate_legacy_index_logs() {
    let new_dir = index_log_dir();
    let temp = std::env::temp_dir();
    migrate_legacy_index_logs_in(&new_dir, &temp);
}

/// Read up to `tail_lines` lines from each per-repo log file under `dir`.
/// When `repo_filter` is `Some`, returns only that repo's log; otherwise
/// returns every file in the log directory in alphabetical order.
///
/// Each entry is `(repo_name, content)` where content is the last
/// `tail_lines` lines joined by newlines. Missing log directory or no
/// matching files yields an empty Vec, not an error -- callers print
/// a friendly "no logs yet" message in that case.
pub fn read_index_logs_in(
    dir: &Path,
    repo_filter: Option<&str>,
    tail_lines: usize,
) -> Result<Vec<(String, String)>> {
    if !dir.is_dir() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    let entries = std::fs::read_dir(dir).map_err(LegionError::Io)?;
    let mut paths: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_file() && p.extension().and_then(|e| e.to_str()) == Some("log"))
        .collect();
    paths.sort();
    for path in paths {
        let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        if let Some(filter) = repo_filter
            && stem != filter
        {
            continue;
        }
        let content = std::fs::read_to_string(&path).map_err(LegionError::Io)?;
        let tail: Vec<&str> = content.lines().rev().take(tail_lines).collect();
        let joined = tail.into_iter().rev().collect::<Vec<_>>().join("\n");
        out.push((stem.to_string(), joined));
    }
    Ok(out)
}

/// Production wrapper: read tails from the XDG_STATE_HOME-rooted
/// directory. Used by the `legion index --logs` CLI handler.
pub fn read_index_logs(
    repo_filter: Option<&str>,
    tail_lines: usize,
) -> Result<Vec<(String, String)>> {
    read_index_logs_in(&index_log_dir(), repo_filter, tail_lines)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::Mutex;
    use tempfile::TempDir;

    /// Serializes every test in this module that mutates `$PATH`. Cargo's
    /// default parallel runner gives no env isolation, and three separate
    /// PATH-mutating tests racing on Ubuntu CI consistently corrupted each
    /// others' subprocess invocations even though every test individually
    /// restored PATH at exit. Each PATH-touching test must lock this mutex
    /// at entry and hold it through the final restore. See
    /// `run_scip_rust_fallback_chain` for the canonical pattern.
    static PATH_TEST_LOCK: Mutex<()> = Mutex::new(());

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
    fn detect_languages_java_for_each_build_marker() {
        for marker in ["pom.xml", "build.gradle", "build.gradle.kts"] {
            let dir = TempDir::new().unwrap();
            fs::write(dir.path().join(marker), "").unwrap();
            assert_eq!(
                detect_languages(dir.path()),
                vec!["java"],
                "marker={marker}"
            );
        }
    }

    #[test]
    fn detect_languages_ruby_for_gemfile() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("Gemfile"), "").unwrap();
        assert_eq!(detect_languages(dir.path()), vec!["ruby"]);
    }

    #[test]
    fn detect_languages_clang_for_either_marker() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("CMakeLists.txt"), "").unwrap();
        assert_eq!(detect_languages(dir.path()), vec!["clang"]);

        let dir2 = TempDir::new().unwrap();
        fs::write(dir2.path().join("compile_commands.json"), "[]").unwrap();
        assert_eq!(detect_languages(dir2.path()), vec!["clang"]);
    }

    #[test]
    fn detect_languages_csharp_for_csproj_glob() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("MyApp.csproj"), "").unwrap();
        assert_eq!(detect_languages(dir.path()), vec!["csharp"]);
    }

    #[test]
    fn detect_languages_csharp_for_sln_glob() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("MyApp.sln"), "").unwrap();
        assert_eq!(detect_languages(dir.path()), vec!["csharp"]);
    }

    #[test]
    fn detect_languages_csharp_ignores_unrelated_extensions() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("notes.txt"), "").unwrap();
        fs::write(dir.path().join("config.json"), "").unwrap();
        assert!(detect_languages(dir.path()).is_empty());
    }

    #[test]
    fn detect_languages_php_for_composer_json() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("composer.json"), "{}").unwrap();
        assert_eq!(detect_languages(dir.path()), vec!["php"]);
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
    fn detect_ts_workspace_flavor_pnpm_for_yaml_marker() {
        let dir = TempDir::new().unwrap();
        fs::write(
            dir.path().join("pnpm-workspace.yaml"),
            "packages:\n  - 'packages/*'\n",
        )
        .unwrap();
        assert_eq!(
            detect_ts_workspace_flavor(dir.path()),
            TsWorkspaceFlavor::Pnpm
        );
    }

    #[test]
    fn detect_ts_workspace_flavor_yarn_for_workspaces_array() {
        let dir = TempDir::new().unwrap();
        fs::write(
            dir.path().join("package.json"),
            r#"{"name":"x","workspaces":["packages/*","apps/*"]}"#,
        )
        .unwrap();
        assert_eq!(
            detect_ts_workspace_flavor(dir.path()),
            TsWorkspaceFlavor::Yarn
        );
    }

    #[test]
    fn detect_ts_workspace_flavor_yarn_for_workspaces_object() {
        let dir = TempDir::new().unwrap();
        fs::write(
            dir.path().join("package.json"),
            r#"{"name":"x","workspaces":{"packages":["packages/*"],"nohoist":["**/react-native"]}}"#,
        )
        .unwrap();
        assert_eq!(
            detect_ts_workspace_flavor(dir.path()),
            TsWorkspaceFlavor::Yarn
        );
    }

    #[test]
    fn detect_ts_workspace_flavor_none_for_package_json_without_workspaces() {
        let dir = TempDir::new().unwrap();
        fs::write(
            dir.path().join("package.json"),
            r#"{"name":"x","version":"1.0.0"}"#,
        )
        .unwrap();
        assert_eq!(
            detect_ts_workspace_flavor(dir.path()),
            TsWorkspaceFlavor::None
        );
    }

    #[test]
    fn detect_ts_workspace_flavor_none_when_no_markers() {
        let dir = TempDir::new().unwrap();
        assert_eq!(
            detect_ts_workspace_flavor(dir.path()),
            TsWorkspaceFlavor::None
        );
    }

    /// Pnpm marker wins when both files exist. Matches pnpm's own resolution:
    /// `pnpm-workspace.yaml` makes the repo a pnpm workspace regardless of
    /// what package.json declares.
    #[test]
    fn detect_ts_workspace_flavor_pnpm_wins_over_package_json_workspaces() {
        let dir = TempDir::new().unwrap();
        fs::write(
            dir.path().join("pnpm-workspace.yaml"),
            "packages:\n  - 'packages/*'\n",
        )
        .unwrap();
        fs::write(
            dir.path().join("package.json"),
            r#"{"name":"x","workspaces":["packages/*"]}"#,
        )
        .unwrap();
        assert_eq!(
            detect_ts_workspace_flavor(dir.path()),
            TsWorkspaceFlavor::Pnpm
        );
    }

    /// Malformed package.json must not panic or fail indexing -- fall through
    /// to None so scip-typescript still runs (it'll surface its own JSON
    /// parse error if the file is actually broken for tsc too).
    #[test]
    fn detect_ts_workspace_flavor_none_for_malformed_package_json() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("package.json"), "not json at all {{{").unwrap();
        assert_eq!(
            detect_ts_workspace_flavor(dir.path()),
            TsWorkspaceFlavor::None
        );
    }

    /// Pin the scip-typescript flag names. The fix for #441 is one strcmp
    /// away from silently regressing if a future refactor renames one of
    /// these flags or drops the `--` prefix -- the production symptom is
    /// the same empty-index gap we're closing here, with no test failure
    /// to catch it. Lock the contract in one place.
    #[test]
    fn scip_typescript_args_pins_flag_names_per_flavor() {
        assert_eq!(
            scip_typescript_args(TsWorkspaceFlavor::Pnpm),
            ["index", "--pnpm-workspaces"]
        );
        assert_eq!(
            scip_typescript_args(TsWorkspaceFlavor::Yarn),
            ["index", "--yarn-workspaces"]
        );
        assert_eq!(scip_typescript_args(TsWorkspaceFlavor::None), ["index"]);
    }

    /// Pin the dispatch table: each supported lang tag must route to a helper
    /// whose missing-binary error names a binary specific to that language.
    /// Prevents a future typo in `run_indexer`'s match arms (e.g. routing
    /// "java" to scip-ruby) from going undetected.
    #[cfg(unix)]
    #[test]
    fn run_indexer_dispatches_each_language_to_its_helper() {
        let _guard = PATH_TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let prior = std::env::var("PATH").unwrap_or_default();
        let (empty_dir, empty_path) = isolate_path_with_shims(&[]);
        let repo = TempDir::new().unwrap();
        // Safety: see run_scip_rust_fallback_chain.
        unsafe {
            std::env::set_var("PATH", &empty_path);
        }

        let cases = [
            ("typescript", "scip-typescript"),
            ("python", "scip-python"),
            ("go", "scip-go"),
            ("java", "scip-java"),
            ("ruby", "scip-ruby"),
            ("clang", "scip-clang"),
            ("csharp", "scip-dotnet"),
            ("php", "scip-php"),
        ];
        let results: Vec<_> = cases
            .iter()
            .map(|(lang, _)| (*lang, run_indexer(lang, repo.path())))
            .collect();

        unsafe {
            std::env::set_var("PATH", &prior);
        }
        drop(empty_dir);

        for ((lang, result), (_, expected_binary)) in results.iter().zip(cases.iter()) {
            match result.as_ref().unwrap_err() {
                LegionError::IndexerNotFound { lang: l, binary } => {
                    assert_eq!(l, lang, "lang field must echo the dispatched lang");
                    assert!(
                        binary.contains(expected_binary),
                        "{lang} dispatched but error names wrong binary: {binary}"
                    );
                }
                other => panic!("expected IndexerNotFound for {lang}, got {other:?}"),
            }
        }
    }

    #[test]
    fn migrate_legacy_index_logs_moves_tmp_files() {
        let new_dir = TempDir::new().unwrap();
        let legacy_dir = TempDir::new().unwrap();

        let legacy = legacy_dir.path().join("legion-index-myrepo.log");
        fs::write(&legacy, "old content\n").unwrap();
        let unrelated = legacy_dir.path().join("other-tool.log");
        fs::write(&unrelated, "leave me alone\n").unwrap();

        let target = new_dir.path().join("index-logs");
        migrate_legacy_index_logs_in(&target, legacy_dir.path());

        let migrated = target.join("myrepo.log");
        assert_eq!(std::fs::read_to_string(&migrated).unwrap(), "old content\n");
        assert!(!legacy.exists(), "legacy file must be moved");
        assert!(unrelated.exists(), "unrelated tmp file must be preserved");

        // Idempotent: a second call with no legacy files left is a no-op.
        migrate_legacy_index_logs_in(&target, legacy_dir.path());
        assert_eq!(std::fs::read_to_string(&migrated).unwrap(), "old content\n");
    }

    #[test]
    fn read_index_logs_returns_empty_when_dir_missing() {
        let dir = TempDir::new().unwrap();
        let missing = dir.path().join("does-not-exist");
        assert!(read_index_logs_in(&missing, None, 50).unwrap().is_empty());
    }

    #[test]
    fn read_index_logs_returns_tail_per_repo_with_filter() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("repoA.log"), "1\n2\n3\n4\n5\n").unwrap();
        fs::write(dir.path().join("repoB.log"), "x\ny\n").unwrap();
        // Files without .log extension are ignored.
        fs::write(dir.path().join("README.md"), "skip me\n").unwrap();

        let unfiltered = read_index_logs_in(dir.path(), None, 3).unwrap();
        assert_eq!(unfiltered.len(), 2);
        assert_eq!(unfiltered[0].0, "repoA");
        assert_eq!(unfiltered[0].1, "3\n4\n5");
        assert_eq!(unfiltered[1].0, "repoB");
        assert_eq!(unfiltered[1].1, "x\ny");

        let filtered = read_index_logs_in(dir.path(), Some("repoA"), 50).unwrap();
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].0, "repoA");
        assert_eq!(filtered[0].1, "1\n2\n3\n4\n5");
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
        let _guard = PATH_TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
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
            (
                "java",
                run_scip_java(repo.path()),
                "scip-java",
                "github.com/sourcegraph/scip-java",
            ),
            (
                "ruby",
                run_scip_ruby(repo.path()),
                "scip-ruby",
                "gem install",
            ),
            (
                "clang",
                run_scip_clang(repo.path()),
                "scip-clang",
                "github.com/sourcegraph/scip-clang/releases",
            ),
            (
                "csharp",
                run_scip_dotnet(repo.path()),
                "scip-dotnet",
                "dotnet tool install",
            ),
            (
                "php",
                run_scip_php(repo.path()),
                "scip-php",
                "composer global require",
            ),
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
        let _guard = PATH_TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
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
        // The transient protobuf is removed once its bytes are captured, so a
        // successful index leaves no stray index.scip in the repo root.
        assert!(
            !repo.path().join("index.scip").exists(),
            "index.scip should be removed from repo root after ingest"
        );
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
