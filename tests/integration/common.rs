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
// Hermetic git fixtures (#723, hardened further by #740).
//
// A handful of quality-gate tests need a real `git` repo (so `git_changed_files`
// has something to probe). Every such invocation must run with an explicit
// tempdir `current_dir` AND set identity via per-invocation `-c` overrides --
// never via a `git config` write -- so a `current_dir` step that silently
// failed to apply could not fall through to writing the ENCLOSING checkout's
// real `.git/config`. That happened live (#723): a fixture's `git config
// user.name` call poisoned the shared config (worktrees inherit it), mis-
// attributing four real commits before it was caught.
//
// #740 (second live occurrence: `core.bare` flipped true on the real checkout
// after a killed test run) adds a second, independent barrier on top of the
// `current_dir` + `-c` discipline: every fixture invocation also pins
// `GIT_CONFIG_GLOBAL`/`GIT_CONFIG_SYSTEM` to isolated empty files and
// `GIT_DIR`/`GIT_WORK_TREE` directly to the fixture repo, so a misfire
// cannot resolve the operator's real global/system git config or any
// directory other than the tempdir even if `current_dir` were somehow wrong.
// ---------------------------------------------------------------------------

static ISOLATED_GIT_CONFIG: OnceLock<(PathBuf, PathBuf)> = OnceLock::new();

/// Suite-wide isolated (always-empty) `GIT_CONFIG_GLOBAL` / `GIT_CONFIG_SYSTEM`
/// file paths. Every fixture git invocation points at these instead of
/// letting git resolve the operator's real `~/.gitconfig` / `/etc/gitconfig`,
/// so even a command that forgets an identity override (or a future call
/// site that never learned the `-c` discipline) cannot read or write real
/// global state. The backing tempdir is deliberately leaked (`mem::forget`):
/// it must outlive every test in this binary, and process exit reclaims it
/// like any other tempfile.
pub fn isolated_git_config_paths() -> &'static (PathBuf, PathBuf) {
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

/// Build a `git` `Command` scoped to `repo_dir`: explicit `current_dir` plus
/// the `GIT_CONFIG_GLOBAL`/`GIT_CONFIG_SYSTEM`/`GIT_DIR`/`GIT_WORK_TREE`
/// isolation described above. Callers still add their own args (and, for
/// writes, their own `-c` identity overrides).
fn fixture_git_command(repo_dir: &Path) -> Command {
    let (global_config, system_config) = isolated_git_config_paths();
    let mut cmd = Command::new("git");
    cmd.current_dir(repo_dir)
        .env("GIT_CONFIG_GLOBAL", global_config)
        .env("GIT_CONFIG_SYSTEM", system_config)
        .env("GIT_DIR", repo_dir.join(".git"))
        .env("GIT_WORK_TREE", repo_dir);
    cmd
}

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
    let out = fixture_git_command(repo_dir)
        .args(&full_args)
        .output()
        .unwrap_or_else(|e| panic!("git {args:?} failed to spawn in {repo_dir:?}: {e}"));
    assert!(
        out.status.success(),
        "git {args:?} exited non-zero in {repo_dir:?}\nstderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Read-only counterpart to `run_git_fixture` for queries whose stdout the
/// caller needs (e.g. `rev-parse HEAD`), sharing the same config/dir
/// isolation so a read can no more resolve the enclosing checkout than a
/// write can. Panics loudly on spawn failure or non-zero exit, same as
/// `run_git_fixture`.
pub fn run_git_fixture_output(repo_dir: &Path, args: &[&str]) -> String {
    let out = fixture_git_command(repo_dir)
        .args(args)
        .output()
        .unwrap_or_else(|e| panic!("git {args:?} failed to spawn in {repo_dir:?}: {e}"));
    assert!(
        out.status.success(),
        "git {args:?} exited non-zero in {repo_dir:?}\nstderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
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

/// Root of the checkout this test crate is compiled from. In a `git
/// worktree add` checkout this is the worktree's own directory -- git still
/// resolves `git config`/`git -C <this> ...` invocations through to the
/// shared main-repo config via the worktree indirection, so it is the
/// correct `current_dir` for both reading and (suite-start repair) writing
/// that shared config.
fn checkout_root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

/// `resolve_git_config_path` applied to the checkout this test crate is
/// compiled from.
fn real_repo_config_path() -> Option<PathBuf> {
    resolve_git_config_path(checkout_root())
}

/// Read a single git config key from `root`'s LOCAL scope only (`git config
/// --local --get`), using git's own parser rather than hand-rolled TOML/ini
/// scanning. Scoping to `--local` matters for two reasons: the corruption
/// this guards against is specifically a write into the checkout's own
/// `.git/config`, not the operator's global config, so that is the only
/// scope worth asking about; and it keeps this check's result independent
/// of whatever the invoking machine's real `~/.gitconfig` happens to
/// contain (an unscoped `--get` would fall through to it and could mask --
/// or spuriously flag -- corruption depending on the operator's own
/// identity). `None` when the key is unset locally or `git config` fails
/// for any reason (e.g. `root` is not resolvable as a repo).
fn query_config_value(root: &Path, key: &str) -> Option<String> {
    let out = Command::new("git")
        .args(["config", "--local", "--get", key])
        .current_dir(root)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Pure decision, isolated from I/O so it is unit-testable without touching
/// any real git state: does `(bare, name, email)` read like the corruption
/// observed live? `bare == Some("true")` is the operationally blocking flip
/// (every git command in the checkout fails with "this operation must be
/// run in a work tree" until it's cleared). `name`/`email` are checked
/// against both the historical pre-#723 poisoning values ("Test" /
/// "test@example.com", from the live incident) and the current
/// `run_git_fixture` literals, in case some future fixture regresses to a
/// real `git config` write. Returns one human-readable string per
/// signature found; empty means clean.
///
/// #740: an operator whose real `user.name` legitimately happens to be
/// "Test" (or whose email is literally "test@example.com") would false-
/// positive here; this is an accepted trade-off given how narrow and
/// unusual that identity is in practice, weighed against the cost of a
/// silently unrepaired real corruption.
fn corruption_signatures(
    bare: Option<&str>,
    name: Option<&str>,
    email: Option<&str>,
) -> Vec<String> {
    let mut found = Vec::new();
    if bare == Some("true") {
        found.push("core.bare=true".to_string());
    }
    let name_poisoned = matches!(name, Some("Test") | Some("Legion Test Fixture"));
    let email_poisoned = matches!(
        email,
        Some("test@example.com") | Some("legion-test-fixture@example.invalid")
    );
    if name_poisoned || email_poisoned {
        found.push(format!(
            "fixture-poisoned identity (user.name={name:?}, user.email={email:?})"
        ));
    }
    found
}

/// Detect known corruption signatures on `root`'s git config and repair
/// `core.bare` (the operationally blocking flip) if found -- a poisoned
/// identity is reported but left untouched, since overwriting someone's
/// name/email is not a call this function should make unilaterally. Panics
/// if `core.bare=true` is found but the repair itself fails, so a failed
/// repair is never silently swallowed. Returns the human-readable findings
/// (empty = clean).
fn detect_and_repair_corruption(root: &Path) -> Vec<String> {
    let bare = query_config_value(root, "core.bare");
    let name = query_config_value(root, "user.name");
    let email = query_config_value(root, "user.email");
    let found = corruption_signatures(bare.as_deref(), name.as_deref(), email.as_deref());

    if found.iter().any(|f| f.starts_with("core.bare")) {
        let repair = Command::new("git")
            .args(["config", "core.bare", "false"])
            .current_dir(root)
            .output();
        match repair {
            Ok(out) if out.status.success() => {}
            Ok(out) => panic!(
                "detected core.bare=true corruption on {root:?} AND failed to repair it: {}",
                String::from_utf8_lossy(&out.stderr)
            ),
            Err(e) => panic!(
                "detected core.bare=true corruption on {root:?} AND failed to spawn the repair: {e}"
            ),
        }
    }

    found
}

static SUITE_START_CHECKED: OnceLock<()> = OnceLock::new();

/// Suite-START corruption check (#740): #723's end-of-test `Drop` guard
/// cannot fire when a test run is killed mid-fixture (agent turn ends,
/// timeout, ctrl-c) -- the process dies before `Drop` ever runs, so a
/// poisoned real checkout persists silently until someone hits a mystery
/// git failure later. This runs once per test binary at the top of
/// `RealRepoConfigGuard::new()`, BEFORE the end-of-suite baseline is
/// captured: if the real checkout's config already shows a known corruption
/// signature, it means a PRIOR suite run left it that way. Repairs
/// `core.bare` and panics loudly, naming exactly what was found, rather
/// than silently adopting the poisoned state as this run's new baseline.
///
/// Uses `OnceLock::get_or_init` (not a swapped `AtomicBool`) so concurrent
/// test threads BLOCK on the first caller's check-and-repair rather than
/// racing past it: an `AtomicBool` swap would let a second thread capture
/// `config_baseline()`'s hash of the still-corrupted file while the first
/// thread's repair is in flight, turning every later `Drop` guard's
/// comparison into a false "changed during this run" report.
fn check_suite_start_for_leftover_corruption() {
    SUITE_START_CHECKED.get_or_init(|| {
        if real_repo_config_path().is_none() {
            return;
        }
        let found = detect_and_repair_corruption(checkout_root());
        if found.is_empty() {
            return;
        }
        // The disposition sentence only claims a repair when `core.bare` was
        // actually part of what was found -- `detect_and_repair_corruption`
        // leaves a poisoned identity untouched (see its doc comment), so an
        // identity-only corruption must not claim a repair that never
        // happened.
        let disposition = if found.iter().any(|f| f.starts_with("core.bare")) {
            "core.bare has been repaired to false; if a poisoned identity was also found, \
             verify user.name/user.email manually before committing."
        } else {
            "No repair was applied (only a poisoned identity was found, not core.bare); \
             verify user.name/user.email manually before committing."
        };
        panic!(
            "enclosing checkout's real .git/config was corrupted BEFORE this suite run started \
             ({}) at {:?} -- a previous integration-suite run was almost certainly killed \
             mid-fixture (#723's end-of-suite guard cannot fire on a killed process). \
             {disposition} See #740.",
            found.join("; "),
            checkout_root(),
        );
    });
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
/// fixture command runs: `new()` first runs the suite-START leftover-
/// corruption check (#740, see `check_suite_start_for_leftover_corruption`),
/// then snapshots (or reuses) the suite-wide baseline hash of the real
/// enclosing checkout's `.git/config`, and `Drop` re-hashes it and panics --
/// naming the file -- if it no longer matches.
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
        check_suite_start_for_leftover_corruption();
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

    // --- suite-start corruption detection (#740) ---

    #[test]
    fn corruption_signatures_clean_state_finds_nothing() {
        assert!(
            corruption_signatures(
                Some("false"),
                Some("Sean Silvius"),
                Some("sean@example.com")
            )
            .is_empty()
        );
        assert!(corruption_signatures(None, None, None).is_empty());
    }

    #[test]
    fn corruption_signatures_flags_bare_true() {
        let found = corruption_signatures(Some("true"), None, None);
        assert_eq!(found, vec!["core.bare=true".to_string()]);
    }

    #[test]
    fn corruption_signatures_flags_historical_test_identity() {
        // The exact pre-#723 live-incident poisoning: "[user] Test/test@example.com".
        let found = corruption_signatures(None, Some("Test"), Some("test@example.com"));
        assert_eq!(found.len(), 1);
        assert!(found[0].contains("poisoned identity"));
    }

    #[test]
    fn corruption_signatures_flags_current_fixture_identity() {
        let found = corruption_signatures(
            None,
            Some("Legion Test Fixture"),
            Some("legion-test-fixture@example.invalid"),
        );
        assert_eq!(found.len(), 1);
    }

    #[test]
    fn corruption_signatures_flags_both_bare_and_identity_independently() {
        let found = corruption_signatures(Some("true"), Some("Test"), Some("test@example.com"));
        assert_eq!(
            found.len(),
            2,
            "expected one entry per independent signature: {found:?}"
        );
    }

    #[test]
    fn query_config_value_returns_none_for_unset_local_key() {
        let repo = tempfile::tempdir().unwrap();
        run_git_fixture(repo.path(), &["init"]);
        // run_git_fixture supplies identity via per-invocation `-c` overrides,
        // never a persisted `git config` write (that is the whole point of
        // #723) -- so the LOCAL scope has no user.name at all here.
        assert_eq!(query_config_value(repo.path(), "user.name"), None);
    }

    /// Write `key = value` into `repo`'s local config, going through
    /// `fixture_git_command` for the same isolation every other fixture
    /// write uses (this is a disposable throwaway repo, never the real
    /// checkout -- it exists precisely to *simulate* corruption to test
    /// the detector against, and that simulation must not itself take the
    /// unpinned shortcut #740 hardens everything else against).
    fn set_fixture_config(repo: &Path, key: &str, value: &str) {
        let out = fixture_git_command(repo)
            .args(["config", key, value])
            .output()
            .unwrap_or_else(|e| {
                panic!("git config {key} {value} failed to spawn in {repo:?}: {e}")
            });
        assert!(
            out.status.success(),
            "git config {key} {value} exited non-zero in {repo:?}\nstderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    #[test]
    fn query_config_value_reads_a_locally_set_key() {
        let repo = tempfile::tempdir().unwrap();
        run_git_fixture(repo.path(), &["init"]);
        set_fixture_config(repo.path(), "core.bare", "true");
        assert_eq!(
            query_config_value(repo.path(), "core.bare").as_deref(),
            Some("true")
        );
    }

    /// `detect_and_repair_corruption` against a deliberately poisoned FAKE
    /// repo (never the real enclosing checkout): finds `core.bare=true` and
    /// repairs it, leaving identity alone since this function must never
    /// unilaterally overwrite someone's name/email.
    #[test]
    fn detect_and_repair_corruption_repairs_bare_and_reports_identity() {
        let repo = tempfile::tempdir().unwrap();
        run_git_fixture(repo.path(), &["init"]);
        // Poison it directly (this is a disposable fixture repo, not the
        // real checkout) to simulate a killed run's leftover state.
        set_fixture_config(repo.path(), "core.bare", "true");
        set_fixture_config(repo.path(), "user.name", "Test");
        set_fixture_config(repo.path(), "user.email", "test@example.com");

        let found = detect_and_repair_corruption(repo.path());
        assert_eq!(
            found.len(),
            2,
            "expected bare + identity signatures: {found:?}"
        );
        assert!(found.iter().any(|f| f.contains("core.bare=true")));
        assert!(found.iter().any(|f| f.contains("poisoned identity")));

        // core.bare must now be repaired to false.
        assert_eq!(
            query_config_value(repo.path(), "core.bare").as_deref(),
            Some("false"),
            "core.bare must be repaired, not merely reported"
        );
        // Identity must be left untouched -- repairing it is not this
        // function's call to make.
        assert_eq!(
            query_config_value(repo.path(), "user.name").as_deref(),
            Some("Test"),
            "identity must be reported, never silently rewritten"
        );
    }

    #[test]
    fn detect_and_repair_corruption_clean_repo_finds_nothing() {
        let repo = tempfile::tempdir().unwrap();
        run_git_fixture(repo.path(), &["init"]);
        assert!(detect_and_repair_corruption(repo.path()).is_empty());
    }
}
