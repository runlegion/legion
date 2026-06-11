//! Work source plugins: discovery, bounded execution, and config.
//! Carved from a single worksource.rs (#615): `issues` and `prs` hold
//! the per-operation wrappers; this file owns the plugin infrastructure
//! (find/resolve/cache, call_plugin + timeout, the plugin_call decode
//! boundary with its fail-closed missing-plugin policy) and watch.toml
//! config resolution.

mod issues;
mod prs;

pub use issues::*;
pub use prs::*;

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Mutex, OnceLock};
use std::thread;
use std::time::Duration;

use wait_timeout::ChildExt;

use crate::error::{LegionError, Result};
/// Per-process cache of resolved work source plugin paths. Each (name) is
/// resolved once via find_plugin_uncached and memoized. Work source commands
/// (issue create, pr create, list, comment, review, merge) all hit find_plugin
/// on every invocation -- the fallback cache scan can cost ~40 stat syscalls,
/// so caching is worth a tiny mutex.
fn plugin_cache() -> &'static Mutex<std::collections::HashMap<String, PathBuf>> {
    static CACHE: OnceLock<Mutex<std::collections::HashMap<String, PathBuf>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(std::collections::HashMap::new()))
}

/// Discover work source plugin paths. Results are memoized per process.
///
/// Resolution order:
/// 1. `CLAUDE_PLUGIN_ROOT/worksources/<name>` -- the env var Claude Code sets
///    when running hooks. Primary path during normal plugin execution.
/// 2. Alongside the current executable, for dev checkouts where the binary
///    lives in `plugin/bin/` next to `worksources/`.
/// 3. Glob `~/.claude/plugins/cache/*/legion/*/worksources/<name>` and pick
///    the highest version that has the worksource. The plugin cache path is
///    predictable regardless of env-var state, so this fallback survives
///    upstream versions that pass an empty `CLAUDE_PLUGIN_ROOT` under Bash
///    subprocess context.
fn find_plugin(name: &str) -> Option<PathBuf> {
    if let Ok(cache) = plugin_cache().lock()
        && let Some(cached) = cache.get(name)
    {
        return Some(cached.clone());
    }

    let resolved = resolve_plugin(name)?;

    if let Ok(mut cache) = plugin_cache().lock() {
        cache.insert(name.to_string(), resolved.clone());
    }
    Some(resolved)
}

/// Uncached plugin resolution. See [`find_plugin`] for the resolution order.
fn resolve_plugin(name: &str) -> Option<PathBuf> {
    // 1. CLAUDE_PLUGIN_ROOT (primary)
    if let Ok(plugin_root) = std::env::var("CLAUDE_PLUGIN_ROOT")
        && !plugin_root.is_empty()
    {
        let path = PathBuf::from(&plugin_root).join("worksources").join(name);
        if path.exists() {
            return Some(path);
        }
    }

    // 2. Alongside the executable (dev checkout: plugin/bin/legion)
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        let path = dir.join("worksources").join(name);
        if path.exists() {
            return Some(path);
        }
        if let Some(grand) = dir.parent() {
            let path = grand.join("worksources").join(name);
            if path.exists() {
                return Some(path);
            }
        }
    }

    // 3. Plugin cache scan: ~/.claude/plugins/cache/*/legion/*/worksources/<name>
    find_in_plugin_cache(name)
}

/// Scan the Claude Code plugin cache for the highest version of legion that
/// ships the requested worksource. Returns None if no cached version has it.
///
/// Defaults to `~/.claude/plugins/cache`; override via [`find_in_cache_root`]
/// for tests.
fn find_in_plugin_cache(name: &str) -> Option<PathBuf> {
    let cache_root = dirs::home_dir()?
        .join(".claude")
        .join("plugins")
        .join("cache");
    find_in_cache_root(&cache_root, name)
}

/// Testable inner: scan a specific cache root directory for the highest
/// version of legion that ships `name`. Separated from `find_in_plugin_cache`
/// so tests can point at a tempdir.
fn find_in_cache_root(cache_root: &Path, name: &str) -> Option<PathBuf> {
    let mut best: Option<(Vec<u32>, PathBuf)> = None;

    // Iterate marketplaces under cache_root.
    for marketplace in std::fs::read_dir(cache_root).ok()?.flatten() {
        let legion_dir = marketplace.path().join("legion");
        let Ok(versions) = std::fs::read_dir(&legion_dir) else {
            continue;
        };
        for version in versions.flatten() {
            let vpath = version.path();
            let candidate = vpath.join("worksources").join(name);
            if !candidate.exists() {
                continue;
            }
            let Some(vname) = vpath.file_name().and_then(|s| s.to_str()) else {
                continue;
            };
            let parsed = parse_semver(vname);
            match &best {
                Some((best_v, _)) if parsed <= *best_v => {}
                _ => best = Some((parsed, candidate)),
            }
        }
    }

    best.map(|(_, p)| p)
}

/// Parse a version string like "0.4.7" into [0, 4, 7] for comparison.
/// Non-numeric segments become 0, which sorts them first. Good enough for the
/// x.y.z scheme we actually ship.
fn parse_semver(v: &str) -> Vec<u32> {
    v.split('.').map(|s| s.parse().unwrap_or(0)).collect()
}

/// Default wall-clock budget for a plugin invocation. GitHub API calls
/// normally complete in under a second; a 30s budget tolerates slow
/// networks while bounding the damage when `gh` hangs on an auth prompt,
/// a DNS stall, or a stuck TLS handshake. `legion watch` and CI runners
/// depend on this not wedging indefinitely.
///
/// Override by setting `LEGION_WS_TIMEOUT_SECS` in the environment. A value
/// of 0 disables the timeout (falls back to the pre-timeout behaviour,
/// primarily for debugging a known-hung plugin under strace).
const DEFAULT_PLUGIN_TIMEOUT_SECS: u64 = 30;

fn resolve_plugin_timeout() -> Option<Duration> {
    let secs = std::env::var("LEGION_WS_TIMEOUT_SECS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(DEFAULT_PLUGIN_TIMEOUT_SECS);
    if secs == 0 {
        None
    } else {
        Some(Duration::from_secs(secs))
    }
}

/// Call a work source plugin with the given subcommand, bounded by a
/// wall-clock timeout so a hung gh call cannot wedge the caller
/// indefinitely. On timeout the child is SIGKILLed and a
/// `LegionError::WorkSource("plugin timeout after Ns")` is returned.
fn call_plugin(plugin_path: &Path, args: &[&str], env: &[(&str, &str)]) -> Result<String> {
    call_plugin_inner(plugin_path, args, env, resolve_plugin_timeout())
}

fn call_plugin_inner(
    plugin_path: &Path,
    args: &[&str],
    env: &[(&str, &str)],
    timeout: Option<Duration>,
) -> Result<String> {
    let mut cmd = Command::new(plugin_path);
    cmd.args(args);
    for (key, val) in env {
        cmd.env(key, val);
    }
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // Child becomes its own process group leader on unix. Required so that
    // on timeout we can kill the entire subtree (`killpg`), not just the
    // bash interpreter. Without this, a bash plugin that spawns `gh` would
    // leave `gh` orphaned and still holding pipe fds open -- reader threads
    // would block on EOF until gh itself exited, defeating the timeout.
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }

    let mut child = cmd
        .spawn()
        .map_err(|e| LegionError::WorkSource(format!("failed to run plugin: {e}")))?;

    // Reader threads must be spawned BEFORE we wait. If the plugin emits
    // more than a pipe buffer of output and nothing is draining, the
    // plugin blocks on write() and wait() never returns -- classic
    // producer/consumer deadlock that `Command::output` avoids by doing
    // exactly this dance internally.
    let stdout_pipe = child
        .stdout
        .take()
        .ok_or_else(|| LegionError::WorkSource("plugin stdout missing".into()))?;
    let stderr_pipe = child
        .stderr
        .take()
        .ok_or_else(|| LegionError::WorkSource("plugin stderr missing".into()))?;
    let stdout_thread = thread::spawn(move || drain_pipe(stdout_pipe));
    let stderr_thread = thread::spawn(move || drain_pipe(stderr_pipe));

    let status = match timeout {
        Some(budget) => match child
            .wait_timeout(budget)
            .map_err(|e| LegionError::WorkSource(format!("plugin wait: {e}")))?
        {
            Some(status) => status,
            None => {
                kill_child_tree(&mut child);
                let _ = child.wait();
                // Reader threads will hit EOF now that the whole subtree
                // is dead. Join to reclaim them cleanly.
                let _ = stdout_thread.join();
                let _ = stderr_thread.join();
                return Err(LegionError::WorkSource(format!(
                    "plugin timeout after {}s",
                    budget.as_secs()
                )));
            }
        },
        None => child
            .wait()
            .map_err(|e| LegionError::WorkSource(format!("plugin wait: {e}")))?,
    };

    let stdout = stdout_thread
        .join()
        .map_err(|_| LegionError::WorkSource("plugin stdout reader panicked".into()))?
        .map_err(|e| LegionError::WorkSource(format!("plugin stdout: {e}")))?;
    let stderr = stderr_thread
        .join()
        .map_err(|_| LegionError::WorkSource("plugin stderr reader panicked".into()))?
        .unwrap_or_default();

    if !status.success() {
        return Err(LegionError::WorkSource(format!("plugin failed: {stderr}")));
    }

    Ok(stdout)
}

/// SIGKILL the entire process group rooted at the plugin child, so orphan
/// grandchildren (e.g., a `gh` subprocess under a bash interpreter) die
/// with their parent. Falls back to killing only the child on non-unix.
fn kill_child_tree(child: &mut std::process::Child) {
    #[cfg(unix)]
    {
        // child.id() returns the pid; because we spawned with process_group(0)
        // the pgid equals the pid. killpg sends SIGKILL to every member of
        // that group. Negative kill() with the pgid has the same effect and
        // avoids bringing libc's constant namespace in scope elsewhere, but
        // killpg is the clearer call.
        let pid = child.id() as libc::pid_t;
        unsafe {
            libc::killpg(pid, libc::SIGKILL);
        }
    }
    #[cfg(not(unix))]
    {
        let _ = child.kill();
    }
}

fn drain_pipe<R: Read>(mut pipe: R) -> std::io::Result<String> {
    let mut buf = String::new();
    pipe.read_to_string(&mut buf)?;
    Ok(buf)
}

/// Resolve a plugin by name, failing closed when it is not installed.
///
/// The missing-plugin policy, decided once for every work source op: a
/// missing plugin is a hard `WorkSource` error. Earlier revisions let the
/// list ops (`list_issues`, `list_prs`, `list_sub_issues`) silently return
/// empty results instead, which made a sync against a missing plugin report
/// "zero issues" as success -- the same quiet-failure shape that left #190
/// open on GitHub for days after its card was marked done (see
/// `close_issue`'s history). Every op now routes through this single check.
fn require_plugin(plugin_name: &str) -> Result<PathBuf> {
    find_plugin(plugin_name)
        .ok_or_else(|| LegionError::WorkSource(format!("plugin not found: {plugin_name}")))
}

/// Run a plugin subcommand and return its raw stdout. The no-decode variant
/// of [`plugin_call`], for ops whose output is empty (close, merge, review)
/// or plain text (CI logs).
fn plugin_call_raw(plugin_name: &str, args: &[&str], env: &[(&str, &str)]) -> Result<String> {
    let plugin_path = require_plugin(plugin_name)?;
    call_plugin(&plugin_path, args, env)
}

/// Run a plugin subcommand and decode its JSON stdout into `T`.
///
/// Collapses the find_plugin -> missing-plugin check -> call_plugin ->
/// serde-decode sequence that every work source op needs.
fn plugin_call<T: serde::de::DeserializeOwned>(
    plugin_name: &str,
    args: &[&str],
    env: &[(&str, &str)],
) -> Result<T> {
    let output = plugin_call_raw(plugin_name, args, env)?;
    serde_json::from_str(&output).map_err(|e| LegionError::WorkSource(e.to_string()))
}

/// Resolve work source config for a repo from watch.toml.
///
/// Returns (plugin_name, source_repo, workdir) if configured.
pub fn resolve_config(legion_repo: &str) -> Option<(String, String, String)> {
    let data_dir = crate::data_dir().ok()?;
    let config_path = data_dir.join("watch.toml");
    let content = std::fs::read_to_string(&config_path).ok()?;
    let config: toml::Table = content.parse().ok()?;

    let repos = config.get("repos")?.as_array()?;
    for repo in repos {
        let Some(name) = repo.get("name").and_then(|v| v.as_str()) else {
            continue;
        };
        if name != legion_repo {
            continue;
        }
        let worksource = repo
            .get("worksource")
            .and_then(|v| v.as_str())
            .unwrap_or("github");
        let github = repo.get("github").and_then(|v| v.as_str());
        let Some(workdir) = repo.get("workdir").and_then(|v| v.as_str()) else {
            continue;
        };

        if let Some(gh) = github {
            return Some((worksource.to_string(), gh.to_string(), workdir.to_string()));
        }
    }

    None
}

/// Resolve work source config for a repo, or fail with the canonical
/// "no work source configured" error.
///
/// The fatal companion to `resolve_config`, for CLI arms that cannot
/// proceed without a configured work source. Best-effort paths
/// (kanban-close propagation, reconcile, the `work` queue sync) keep
/// calling `resolve_config` directly because an unconfigured repo is a
/// skip for them, not an error. This function is the single source of the
/// operator-facing error text; integration tests assert its prefix
/// (`no work source configured`), so changing the wording here is a
/// test-visible change.
pub fn require_worksource(legion_repo: &str) -> Result<(String, String, String)> {
    resolve_config(legion_repo).ok_or_else(|| {
        LegionError::WorkSource(format!(
            "no work source configured for repo '{legion_repo}' in watch.toml"
        ))
    })
}

/// Review agent configuration from watch.toml.
pub struct ReviewConfig {
    pub agent: String,
    pub auto_signal: bool,
}

/// Read the review agent configuration from watch.toml.
pub fn resolve_review_config() -> Option<ReviewConfig> {
    let data_dir = crate::data_dir().ok()?;
    let config_path = data_dir.join("watch.toml");
    let content = std::fs::read_to_string(&config_path).ok()?;
    let config: toml::Table = content.parse().ok()?;

    let review = config.get("review")?;
    let agent = review.get("agent")?.as_str()?.to_string();
    let auto_signal = review
        .get("auto_signal")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    Some(ReviewConfig { agent, auto_signal })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn find_plugin_returns_none_for_nonexistent() {
        let result = find_plugin("nonexistent-plugin-xyz");
        assert!(result.is_none());
    }

    #[test]
    fn parse_semver_orders_versions() {
        assert!(parse_semver("0.4.7") > parse_semver("0.4.3"));
        assert!(parse_semver("0.5.0") > parse_semver("0.4.99"));
        assert!(parse_semver("1.0.0") > parse_semver("0.99.99"));
        assert_eq!(parse_semver("0.4.7"), vec![0, 4, 7]);
        assert_eq!(parse_semver("garbage"), vec![0]);
    }

    /// Seed a fake plugin cache layout at `root`: `<marketplace>/legion/<version>/worksources/<name>`.
    fn seed_cache(root: &Path, marketplace: &str, version: &str, worksources: &[&str]) {
        let version_dir = root
            .join(marketplace)
            .join("legion")
            .join(version)
            .join("worksources");
        std::fs::create_dir_all(&version_dir).expect("create cache dirs");
        for name in worksources {
            std::fs::write(version_dir.join(name), "#!/bin/sh\n").expect("write worksource");
        }
    }

    #[test]
    fn find_in_cache_root_returns_none_when_empty() {
        let tmp = tempfile::tempdir().expect("tempdir");
        assert!(find_in_cache_root(tmp.path(), "github").is_none());
    }

    #[test]
    fn find_in_cache_root_picks_highest_version() {
        let tmp = tempfile::tempdir().expect("tempdir");
        seed_cache(tmp.path(), "legion", "0.4.3", &["github"]);
        seed_cache(tmp.path(), "legion", "0.5.0", &["github"]);
        seed_cache(tmp.path(), "legion", "0.4.7", &["github"]);

        let found = find_in_cache_root(tmp.path(), "github").expect("should find github");
        assert!(
            found.to_string_lossy().contains("0.5.0"),
            "expected 0.5.0, got {}",
            found.display()
        );
    }

    #[test]
    fn find_in_cache_root_skips_versions_without_worksource() {
        let tmp = tempfile::tempdir().expect("tempdir");
        // 0.5.0 has no github worksource, 0.4.3 does -- should return 0.4.3
        seed_cache(tmp.path(), "legion", "0.5.0", &[]);
        seed_cache(tmp.path(), "legion", "0.4.3", &["github"]);

        let found = find_in_cache_root(tmp.path(), "github").expect("should find github");
        assert!(found.to_string_lossy().contains("0.4.3"));
    }

    #[test]
    fn find_in_cache_root_handles_multiple_marketplaces() {
        let tmp = tempfile::tempdir().expect("tempdir");
        seed_cache(tmp.path(), "marketplace-a", "0.4.3", &["github"]);
        seed_cache(tmp.path(), "marketplace-b", "0.5.0", &["github"]);

        let found = find_in_cache_root(tmp.path(), "github").expect("should find github");
        assert!(found.to_string_lossy().contains("0.5.0"));
    }

    #[test]
    fn find_in_cache_root_returns_none_when_worksource_missing() {
        let tmp = tempfile::tempdir().expect("tempdir");
        seed_cache(tmp.path(), "legion", "0.5.0", &["linear"]);
        assert!(find_in_cache_root(tmp.path(), "github").is_none());
    }

    #[cfg(unix)]
    mod call_plugin_timeout {
        use super::*;
        use std::fs;
        use std::os::unix::fs::PermissionsExt;

        /// Write a throwaway bash script and chmod it executable. Returns
        /// (tempdir holding the script, path to the script).
        fn write_stub(body: &str) -> (tempfile::TempDir, PathBuf) {
            let dir = tempfile::tempdir().expect("tempdir");
            let path = dir.path().join("stub");
            fs::write(&path, body).expect("write stub");
            let mut perm = fs::metadata(&path).expect("stat").permissions();
            perm.set_mode(0o755);
            fs::set_permissions(&path, perm).expect("chmod");
            (dir, path)
        }

        #[test]
        fn returns_stdout_on_success() {
            let (_tmp, path) = write_stub("#!/bin/bash\necho hello\n");
            let out = call_plugin_inner(&path, &[], &[], Some(Duration::from_secs(2)))
                .expect("stub should succeed");
            assert_eq!(out, "hello\n");
        }

        #[test]
        fn surfaces_stderr_on_nonzero_exit() {
            let (_tmp, path) = write_stub("#!/bin/bash\necho bad >&2\nexit 1\n");
            let err = call_plugin_inner(&path, &[], &[], Some(Duration::from_secs(2)))
                .expect_err("nonzero exit must error");
            let LegionError::WorkSource(msg) = err else {
                panic!("expected WorkSource error, got {err:?}")
            };
            assert!(msg.contains("bad"), "expected stderr in error: {msg}");
        }

        #[test]
        fn times_out_and_kills_hung_plugin() {
            // `sleep 60` is well beyond our 1s budget; the timeout arm must
            // fire and SIGKILL the child. Verified via wall-clock -- the call
            // must not take anywhere near 60s.
            let (_tmp, path) = write_stub("#!/bin/bash\nsleep 60\n");
            let start = std::time::Instant::now();
            let err = call_plugin_inner(&path, &[], &[], Some(Duration::from_secs(1)))
                .expect_err("hung plugin must error");
            let elapsed = start.elapsed();
            assert!(
                elapsed < Duration::from_secs(5),
                "timeout fired too late: {elapsed:?}"
            );
            let LegionError::WorkSource(msg) = err else {
                panic!("expected WorkSource error, got {err:?}")
            };
            assert!(
                msg.contains("timeout"),
                "expected timeout error message, got: {msg}"
            );
        }

        #[test]
        fn no_timeout_lets_plugin_run_to_completion() {
            // None budget disables the timeout. Short-running plugin must
            // still return its stdout.
            let (_tmp, path) = write_stub("#!/bin/bash\nsleep 0.1\necho done\n");
            let out = call_plugin_inner(&path, &[], &[], None).expect("no-timeout must succeed");
            assert_eq!(out, "done\n");
        }

        #[test]
        fn survives_stdout_larger_than_pipe_buffer() {
            // Reader threads are spawned BEFORE wait; without them a plugin
            // that writes more than one pipe buffer's worth (~64K on Linux,
            // ~16K on macOS) would deadlock because wait() blocks waiting
            // for exit and the plugin blocks on write(). Emit ~256K and
            // assert we read all of it.
            let (_tmp, path) = write_stub("#!/bin/bash\nyes A | head -c 262144\n");
            let out = call_plugin_inner(&path, &[], &[], Some(Duration::from_secs(5)))
                .expect("large stdout must not deadlock");
            assert_eq!(out.len(), 262144);
        }
    }
}
