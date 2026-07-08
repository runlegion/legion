//! Shared helpers for the legion integration test crate.
//!
//! Every test in this crate exercises the compiled `legion` binary as a
//! subprocess via `CARGO_BIN_EXE_legion`. The spawn-and-assert plumbing
//! lives here so the ~250 call sites do not each hand-roll the
//! stderr-surfacing failure message (#608).

use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::OnceLock;

/// Base `legion` command with `LEGION_DATA_DIR` pointed at an isolated dir.
pub fn legion_cmd(data_dir: &Path) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_legion"));
    cmd.env("LEGION_DATA_DIR", data_dir);
    cmd
}

/// A variant of `legion_cmd` that also overrides the home directory so the
/// usage command reads sessions from the tempdir instead of the real
/// `~/.claude/projects/`.
///
/// `dirs::home_dir()` on Windows uses `SHGetKnownFolderPath(FOLDERID_Profile)`
/// and ignores both `HOME` and `USERPROFILE`, so the usage handler honors a
/// `LEGION_HOME` override that the test sets here.
pub fn legion_cmd_with_home(data_dir: &Path, home_dir: &Path) -> Command {
    let mut cmd = legion_cmd(data_dir);
    cmd.env("LEGION_HOME", home_dir);
    cmd
}

/// Run the command and require success, returning the raw `Output`.
/// Shared body of `run_ok` / `run_ok_stderr`; the panic message surfaces
/// both streams so no call site has to re-type the diagnostic by hand.
fn output_ok(cmd: &mut Command) -> Output {
    let out = cmd.output().expect("failed to execute legion binary");
    assert!(
        out.status.success(),
        "command failed (status {:?})\nstderr:\n{}\nstdout:\n{}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr),
        String::from_utf8_lossy(&out.stdout),
    );
    out
}

/// Run the command, require success, and return stdout as a `String`.
///
/// Panics with the child's stderr (and stdout) when the command fails, so
/// every call site gets the diagnostic message the old hand-rolled
/// `assert!(out.status.success(), "...: {}", from_utf8_lossy(&out.stderr))`
/// idiom used to re-type by hand.
pub fn run_ok(cmd: &mut Command) -> String {
    let out = output_ok(cmd);
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// Run the command, require success, and return stderr as a `String`.
///
/// Several legion surfaces print their human-facing confirmation to stderr
/// (`eprintln!`) while keeping stdout quiet for piping; tests asserting on
/// those messages use this variant.
pub fn run_ok_stderr(cmd: &mut Command) -> String {
    let out = output_ok(cmd);
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// Run the command, require success, and return the raw `Output` so the
/// caller can assert on stdout and stderr from a single invocation --
/// `run_ok`/`run_ok_stderr` each only see half the picture across two
/// separate spawns, which matters when a test needs to pin both a JSON
/// payload on stdout and a diagnostic on stderr from the same run.
pub fn run_ok_output(cmd: &mut Command) -> Output {
    output_ok(cmd)
}

/// Run the command, require failure (non-zero exit), and return
/// `(stdout, stderr)` so the caller can assert on the error surface.
pub fn run_fail(cmd: &mut Command) -> (String, String) {
    let out = cmd.output().expect("failed to execute legion binary");
    assert!(
        !out.status.success(),
        "expected the command to fail but it succeeded\nstdout:\n{}",
        String::from_utf8_lossy(&out.stdout),
    );
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// Spawn the command with a piped stdin, write `payload`, close the pipe,
/// and wait for the child. Returns the raw `Output` -- callers assert
/// success or failure themselves (stdin-fed commands are used for both
/// happy-path seeding and malformed-payload rejection tests).
///
/// The pipe discipline matters: stdin must be dropped before waiting or the
/// child blocks on EOF forever. This helper owns that easy-to-miss step.
pub fn run_with_stdin(cmd: &mut Command, payload: &[u8]) -> Output {
    use std::io::Write;
    let mut child = cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn legion binary");
    child
        .stdin
        .take()
        .expect("child stdin is piped")
        .write_all(payload)
        .expect("failed to write payload to child stdin");
    // The taken stdin handle dropped at the end of the statement above,
    // closing the pipe so the child sees EOF.
    child.wait_with_output().expect("failed to wait for child")
}

/// Drive the schema migrations to completion with one synchronous CLI call.
///
/// Legion's schema migrations are not concurrency-safe at first-open time:
/// two processes racing to ALTER TABLE on a fresh DB produce "duplicate
/// column name" errors. Any test that runs more than one legion process
/// against the same data dir must warm the schema first. The MCP push
/// bridge test documents the original race and keeps its own inline warmup
/// (it moves untouched per the #608 audit); every new multi-process test
/// should call this instead.
pub fn warm_schema(data_dir: &Path) {
    run_ok(legion_cmd(data_dir).args(["post", "--repo", "warmup-repo", "--text", "schema warmup"]));
}

/// Assert `s` is a well-formed UUID of version 7 (legion's ID format).
/// Tolerates surrounding whitespace so raw stdout can be passed directly.
pub fn assert_uuid_format(s: &str) {
    let parsed = uuid::Uuid::parse_str(s.trim())
        .unwrap_or_else(|e| panic!("expected a UUID, got {s:?}: {e}"));
    assert_eq!(
        parsed.get_version_num(),
        7,
        "expected a UUIDv7, got version {} in {s:?}",
        parsed.get_version_num()
    );
}

/// Seed a single rate-limit sample by invoking `legion statusline` with a
/// minimal synthetic Claude Code JSON payload. Returns the path to a
/// transcript file we do NOT create -- statusline tolerates a missing
/// transcript (skips usage sample) and still writes the rate-limit row.
pub fn seed_rate_limit_sample(data_dir: &Path, five_hour_pct: f64, seven_day_pct: f64) {
    let session_id = format!("seed-{}", uuid::Uuid::now_v7());
    let payload = serde_json::json!({
        "session_id": session_id,
        "rate_limits": {
            "five_hour": { "used_percentage": five_hour_pct, "resets_at": 0 },
            "seven_day": { "used_percentage": seven_day_pct, "resets_at": 0 },
        },
    });
    let out = run_with_stdin(
        legion_cmd(data_dir).args(["statusline"]),
        payload.to_string().as_bytes(),
    );
    assert!(
        out.status.success(),
        "statusline seed failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

// ---------------------------------------------------------------------------
// Hermetic git fixtures (#723).
//
// A handful of quality-gate tests need a real `git` repo (so `git_changed_files`
// has something to probe). Every such invocation must run with an explicit
// tempdir `current_dir` AND set identity via per-invocation `-c` overrides --
// never via a `git config` write -- so a `current_dir` step that silently
// failed to apply could not fall through to writing the ENCLOSING checkout's
// real `.git/config`. That happened live (#723): a fixture's `git config
// user.name` call poisoned the shared config (worktrees inherit it), mis-
// attributing four real commits before it was caught.
// ---------------------------------------------------------------------------

/// Run `git` against a fixture repo with identity and signing baked in as
/// per-invocation `-c` overrides (never as a `git config` write) plus an
/// explicit `current_dir`. Panics loudly, naming the failed command and the
/// directory it ran in, rather than ever falling through to the process cwd.
pub fn run_git_fixture(repo_dir: &Path, args: &[&str]) {
    let mut full_args: Vec<&str> = vec![
        "-c",
        "user.name=Legion Test Fixture",
        "-c",
        "user.email=legion-test-fixture@example.invalid",
        "-c",
        "commit.gpgsign=false",
    ];
    full_args.extend_from_slice(args);
    let out = Command::new("git")
        .args(&full_args)
        .current_dir(repo_dir)
        .output()
        .unwrap_or_else(|e| panic!("git {args:?} failed to spawn in {repo_dir:?}: {e}"));
    assert!(
        out.status.success(),
        "git {args:?} exited non-zero in {repo_dir:?}\nstderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Resolve the real `.git/config` given a checkout root, following the
/// worktree indirection: in a `git worktree add` checkout `.git` is a FILE
/// naming a `<main-repo>/.git/worktrees/<name>` gitdir, not a directory, and
/// the actual shared config lives two levels up from there. Returns `None`
/// when no `.git` can be resolved at all (e.g. a source tarball with no git
/// metadata) -- the guard is inert in that case rather than failing an
/// environment it cannot protect. Pure function of `checkout_root` so the
/// worktree-vs-plain-repo branches are unit-testable without touching this
/// crate's own real `.git`.
fn resolve_git_config_path(checkout_root: &Path) -> Option<PathBuf> {
    let git_path = checkout_root.join(".git");
    if git_path.is_dir() {
        return Some(git_path.join("config"));
    }
    let contents = std::fs::read_to_string(&git_path).ok()?;
    let gitdir = contents.strip_prefix("gitdir:")?.trim();
    Some(Path::new(gitdir).join("..").join("..").join("config"))
}

/// `resolve_git_config_path` applied to the checkout this test crate is
/// compiled from.
fn real_repo_config_path() -> Option<PathBuf> {
    resolve_git_config_path(Path::new(env!("CARGO_MANIFEST_DIR")))
}

/// SHA-256 of the file at `path`, lowercase hex. `None` if it cannot be read.
fn hash_config_file(path: &Path) -> Option<String> {
    let bytes = std::fs::read(path).ok()?;
    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    Some(hex::encode(hasher.finalize()))
}

static CONFIG_BASELINE: OnceLock<Option<String>> = OnceLock::new();

/// Suite-wide baseline hash of the real repo's `.git/config`, captured the
/// first time any test asks (via `get_or_init`, safe under parallel test
/// threads). Every later call compares against this same snapshot.
fn config_baseline() -> &'static Option<String> {
    CONFIG_BASELINE.get_or_init(|| real_repo_config_path().and_then(|p| hash_config_file(&p)))
}

/// RAII suite guard against `.git/config` poisoning (#723). Construct one at
/// the top of every test that drives real `git` fixture processes, before any
/// fixture command runs: `new()` snapshots (or reuses) the suite-wide
/// baseline hash of the real enclosing checkout's `.git/config`, and `Drop`
/// re-hashes it and panics -- naming the file -- if it no longer matches.
///
/// Skips the check (never panics) when the real config cannot be resolved or
/// read at all, and when already unwinding from a separate test failure (a
/// panic during unwind would abort the whole suite instead of reporting the
/// original failure).
pub struct RealRepoConfigGuard {
    _private: (),
}

impl RealRepoConfigGuard {
    pub fn new() -> Self {
        let _ = config_baseline();
        Self { _private: () }
    }
}

impl Default for RealRepoConfigGuard {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for RealRepoConfigGuard {
    fn drop(&mut self) {
        if std::thread::panicking() {
            return;
        }
        let Some(expected) = config_baseline() else {
            return;
        };
        let actual = real_repo_config_path().and_then(|p| hash_config_file(&p));
        assert_eq!(
            actual.as_deref(),
            Some(expected.as_str()),
            "real repo .git/config changed during the integration suite run -- a git \
             fixture wrote to the enclosing checkout instead of its own tempdir (#723)"
        );
    }
}

#[cfg(test)]
mod config_guard_tests {
    use super::*;

    /// Plain (non-worktree) checkout: `.git` is a directory, config resolves
    /// to `.git/config` directly.
    #[test]
    fn resolve_git_config_path_plain_repo() {
        let root = tempfile::tempdir().unwrap();
        let git_dir = root.path().join(".git");
        std::fs::create_dir_all(&git_dir).unwrap();
        std::fs::write(git_dir.join("config"), "[core]\n").unwrap();

        let resolved = resolve_git_config_path(root.path()).expect("should resolve");
        assert_eq!(resolved, git_dir.join("config"));
    }

    /// Worktree checkout: `.git` is a FILE pointing at
    /// `<main>/.git/worktrees/<name>`; the shared config lives two levels up,
    /// at `<main>/.git/config`. This is the exact shape that let a fixture
    /// commit poison the real repo (#723) -- worktrees inherit the main
    /// repo's config, so the resolved path must land on the main repo's
    /// `config`, not anything worktree-local.
    #[test]
    fn resolve_git_config_path_worktree() {
        let main_repo = tempfile::tempdir().unwrap();
        let worktree_gitdir = main_repo.path().join(".git/worktrees/feat-x");
        std::fs::create_dir_all(&worktree_gitdir).unwrap();
        std::fs::write(main_repo.path().join(".git/config"), "[core]\n").unwrap();

        let worktree_root = tempfile::tempdir().unwrap();
        std::fs::write(
            worktree_root.path().join(".git"),
            format!("gitdir: {}\n", worktree_gitdir.display()),
        )
        .unwrap();

        let resolved = resolve_git_config_path(worktree_root.path()).expect("should resolve");
        let canonical_resolved = resolved.canonicalize().expect("resolved path must exist");
        let canonical_expected = main_repo
            .path()
            .join(".git/config")
            .canonicalize()
            .expect("expected path must exist");
        assert_eq!(canonical_resolved, canonical_expected);
    }

    /// No `.git` at all (e.g. a source tarball): resolves to `None` rather
    /// than panicking or fabricating a path.
    #[test]
    fn resolve_git_config_path_missing_git_returns_none() {
        let root = tempfile::tempdir().unwrap();
        assert_eq!(resolve_git_config_path(root.path()), None);
    }

    #[test]
    fn hash_config_file_missing_file_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(hash_config_file(&dir.path().join("nope")), None);
    }

    #[test]
    fn hash_config_file_changes_when_contents_change() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config");
        std::fs::write(&path, "[core]\nbare = false\n").unwrap();
        let before = hash_config_file(&path).expect("file exists");

        std::fs::write(&path, "[core]\nbare = true\n").unwrap();
        let after = hash_config_file(&path).expect("file exists");

        assert_ne!(
            before, after,
            "hash must change when the underlying file contents change"
        );
    }

    #[test]
    fn hash_config_file_stable_for_identical_contents() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config");
        std::fs::write(&path, "[user]\nname = Test\n").unwrap();

        let first = hash_config_file(&path).expect("file exists");
        let second = hash_config_file(&path).expect("file exists");
        assert_eq!(first, second);
    }
}
