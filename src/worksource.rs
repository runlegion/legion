use std::collections::HashSet;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Mutex, OnceLock};
use std::thread;
use std::time::Duration;

use wait_timeout::ChildExt;

use crate::db::Database;
use crate::error::{LegionError, Result};
use crate::kanban;

/// Per-process cache of resolved work source plugin paths. Each (name) is
/// resolved once via find_plugin_uncached and memoized. Work source commands
/// (issue create, pr create, list, comment, review, merge) all hit find_plugin
/// on every invocation -- the fallback cache scan can cost ~40 stat syscalls,
/// so caching is worth a tiny mutex.
fn plugin_cache() -> &'static Mutex<std::collections::HashMap<String, PathBuf>> {
    static CACHE: OnceLock<Mutex<std::collections::HashMap<String, PathBuf>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(std::collections::HashMap::new()))
}

/// An issue from an external work tracker.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ExternalIssue {
    pub url: String,
    pub number: u64,
    pub title: String,
    pub body: Option<String>,
    pub labels: Vec<serde_json::Value>,
    pub assignees: Option<Vec<serde_json::Value>>,
    pub state: String,
    #[serde(default, alias = "createdAt")]
    pub created_at: Option<String>,
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

/// List issues from a work source plugin.
pub fn list_issues(
    plugin_name: &str,
    github_repo: &str,
    workdir: &str,
) -> Result<Vec<ExternalIssue>> {
    let plugin_path = match find_plugin(plugin_name) {
        Some(p) => p,
        None => return Ok(Vec::new()),
    };

    let output = call_plugin(
        &plugin_path,
        &["list"],
        &[
            ("LEGION_WS_REPO", github_repo),
            ("LEGION_WS_WORKDIR", workdir),
        ],
    )?;

    let issues: Vec<ExternalIssue> =
        serde_json::from_str(&output).map_err(|e| LegionError::WorkSource(e.to_string()))?;

    Ok(issues)
}

/// View a single issue via a work source plugin.
pub fn view_issue(plugin_name: &str, github_repo: &str, number: u64) -> Result<ExternalIssue> {
    let plugin_path = find_plugin(plugin_name)
        .ok_or_else(|| LegionError::WorkSource(format!("plugin not found: {plugin_name}")))?;

    let output = call_plugin(
        &plugin_path,
        &["view-issue"],
        &[
            ("LEGION_WS_REPO", github_repo),
            ("LEGION_WS_NUMBER", &number.to_string()),
        ],
    )?;

    let issue: ExternalIssue =
        serde_json::from_str(&output).map_err(|e| LegionError::WorkSource(e.to_string()))?;

    Ok(issue)
}

/// Close an issue via a work source plugin.
///
/// When `comment` is provided, the plugin posts it as a closing comment on
/// the issue before transitioning state. The plugin reads the comment from
/// the `LEGION_WS_COMMENT` environment variable and is expected to no-op on
/// the comment step if the variable is empty or absent.
///
/// A missing plugin is a hard error. A previous revision silently returned
/// `Ok(())` when the plugin could not be found, which looked like a
/// successful close from the caller's perspective and let kanban-done
/// transitions claim they had closed the GitHub issue when in fact nothing
/// had happened. That quiet-failure mode was the root cause of #190 sitting
/// open on GitHub for days after its card was marked done.
pub fn close_issue(
    plugin_name: &str,
    github_repo: &str,
    number: u64,
    comment: Option<&str>,
) -> Result<()> {
    let plugin_path = find_plugin(plugin_name)
        .ok_or_else(|| LegionError::WorkSource(format!("plugin not found: {plugin_name}")))?;

    let number_str = number.to_string();
    let mut env: Vec<(&str, &str)> = vec![
        ("LEGION_WS_REPO", github_repo),
        ("LEGION_WS_NUMBER", &number_str),
    ];
    if let Some(c) = comment {
        env.push(("LEGION_WS_COMMENT", c));
    }

    call_plugin(&plugin_path, &["close", &number_str], &env)?;

    Ok(())
}

/// Reopen a previously closed issue via a work source plugin.
///
/// Symmetrical with `close_issue`. When `comment` is provided, the plugin
/// posts it as a reopening comment on the issue after transitioning state
/// back to open. Used to revert a kanban transition that already propagated
/// to GitHub (e.g. a card moved from `done` back to `in-progress`).
///
/// A missing plugin is a hard error, matching the close path.
pub fn reopen_issue(
    plugin_name: &str,
    github_repo: &str,
    number: u64,
    comment: Option<&str>,
) -> Result<()> {
    let plugin_path = find_plugin(plugin_name)
        .ok_or_else(|| LegionError::WorkSource(format!("plugin not found: {plugin_name}")))?;

    let number_str = number.to_string();
    let mut env: Vec<(&str, &str)> = vec![
        ("LEGION_WS_REPO", github_repo),
        ("LEGION_WS_NUMBER", &number_str),
    ];
    if let Some(c) = comment {
        env.push(("LEGION_WS_COMMENT", c));
    }

    call_plugin(&plugin_path, &["reopen", &number_str], &env)?;

    Ok(())
}

/// Edit an existing issue's title and/or body via a work source plugin.
///
/// At least one of `title` or `body` must be set; passing both `None` is
/// rejected as a caller bug rather than silently no-opping. The plugin
/// reads `LEGION_WS_TITLE` and `LEGION_WS_BODY` env vars and is expected
/// to no-op on any field whose env var is empty or absent.
///
/// Used for scope amendments and stale-content fixes after a sync, so
/// agents do not have to drop scope addenda into comment threads where
/// they are buried below fold on the public GitHub view.
pub fn edit_issue(
    plugin_name: &str,
    github_repo: &str,
    number: u64,
    title: Option<&str>,
    body: Option<&str>,
) -> Result<()> {
    if title.is_none() && body.is_none() {
        return Err(LegionError::WorkSource(
            "edit_issue: at least one of title or body must be provided".to_string(),
        ));
    }

    let plugin_path = find_plugin(plugin_name)
        .ok_or_else(|| LegionError::WorkSource(format!("plugin not found: {plugin_name}")))?;

    let number_str = number.to_string();
    let mut env: Vec<(&str, &str)> = vec![
        ("LEGION_WS_REPO", github_repo),
        ("LEGION_WS_NUMBER", &number_str),
    ];
    if let Some(t) = title {
        env.push(("LEGION_WS_TITLE", t));
    }
    if let Some(b) = body {
        env.push(("LEGION_WS_BODY", b));
    }

    call_plugin(&plugin_path, &["edit-issue"], &env)?;

    Ok(())
}

/// Result of creating an issue via a work source plugin.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CreatedIssue {
    pub url: String,
    pub number: u64,
}

/// Create an issue via a work source plugin.
/// Create a child issue linked to a parent issue as a native sub-issue
/// via the GitHub GraphQL `addSubIssue` mutation (#462). Returns the
/// child issue's `CreatedIssue` shape.
///
/// Plugin implementation looks up the parent's node id first, creates
/// the child via `gh issue create`, looks up the child's node id, then
/// calls the addSubIssue mutation. If the parent does not exist, the
/// plugin errors out BEFORE creating the child so we don't leave an
/// orphan. If addSubIssue fails (sub-issues disabled on the repo), the
/// child issue still exists; the plugin warns to stderr but returns
/// success so the caller has the issue reference.
pub fn create_sub_issue(
    plugin_name: &str,
    github_repo: &str,
    parent_number: u64,
    title: &str,
    body: Option<&str>,
) -> Result<CreatedIssue> {
    let plugin_path = find_plugin(plugin_name)
        .ok_or_else(|| LegionError::WorkSource(format!("plugin not found: {plugin_name}")))?;
    let parent_str = parent_number.to_string();
    let mut env: Vec<(&str, &str)> = vec![
        ("LEGION_WS_REPO", github_repo),
        ("LEGION_WS_PARENT_NUMBER", &parent_str),
        ("LEGION_WS_TITLE", title),
    ];
    if let Some(b) = body {
        env.push(("LEGION_WS_BODY", b));
    }
    let output = call_plugin(&plugin_path, &["create-sub-issue"], &env)?;
    let created: CreatedIssue =
        serde_json::from_str(&output).map_err(|e| LegionError::WorkSource(e.to_string()))?;
    Ok(created)
}

/// List sub-issues of a parent issue (#462). State filter is one of
/// `open` / `closed` / `all`; defaults to `open` when None.
pub fn list_sub_issues(
    plugin_name: &str,
    github_repo: &str,
    parent_number: u64,
    state: Option<&str>,
) -> Result<Vec<ExternalIssue>> {
    let plugin_path = match find_plugin(plugin_name) {
        Some(p) => p,
        None => return Ok(Vec::new()),
    };
    let parent_str = parent_number.to_string();
    let state_str = state.unwrap_or("open");
    let env: Vec<(&str, &str)> = vec![
        ("LEGION_WS_REPO", github_repo),
        ("LEGION_WS_PARENT_NUMBER", &parent_str),
        ("LEGION_WS_STATE", state_str),
    ];
    let output = call_plugin(&plugin_path, &["list-sub-issues"], &env)?;
    let issues: Vec<ExternalIssue> =
        serde_json::from_str(&output).map_err(|e| LegionError::WorkSource(e.to_string()))?;
    Ok(issues)
}

pub fn create_issue(
    plugin_name: &str,
    github_repo: &str,
    title: &str,
    body: Option<&str>,
    labels: Option<&str>,
    assignee: Option<&str>,
) -> Result<CreatedIssue> {
    let plugin_path = find_plugin(plugin_name)
        .ok_or_else(|| LegionError::WorkSource(format!("plugin not found: {plugin_name}")))?;

    let mut env: Vec<(&str, &str)> =
        vec![("LEGION_WS_REPO", github_repo), ("LEGION_WS_TITLE", title)];
    if let Some(b) = body {
        env.push(("LEGION_WS_BODY", b));
    }
    if let Some(l) = labels {
        env.push(("LEGION_WS_LABELS", l));
    }
    if let Some(a) = assignee {
        env.push(("LEGION_WS_ASSIGNEE", a));
    }

    let output = call_plugin(&plugin_path, &["create-issue"], &env)?;
    let created: CreatedIssue =
        serde_json::from_str(&output).map_err(|e| LegionError::WorkSource(e.to_string()))?;

    Ok(created)
}

/// Result of creating a PR via a work source plugin.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CreatedPR {
    pub url: String,
    pub number: u64,
}

/// Create a PR via a work source plugin.
#[allow(clippy::too_many_arguments)]
pub fn create_pr(
    plugin_name: &str,
    github_repo: &str,
    title: &str,
    body: Option<&str>,
    base: Option<&str>,
    head: Option<&str>,
    draft: bool,
    labels: Option<&str>,
    reviewer: Option<&str>,
) -> Result<CreatedPR> {
    let plugin_path = find_plugin(plugin_name)
        .ok_or_else(|| LegionError::WorkSource(format!("plugin not found: {plugin_name}")))?;

    let mut env: Vec<(&str, &str)> =
        vec![("LEGION_WS_REPO", github_repo), ("LEGION_WS_TITLE", title)];
    if let Some(b) = body {
        env.push(("LEGION_WS_BODY", b));
    }
    if let Some(b) = base {
        env.push(("LEGION_WS_BASE", b));
    }
    if let Some(h) = head {
        env.push(("LEGION_WS_HEAD", h));
    }
    if draft {
        env.push(("LEGION_WS_DRAFT", "true"));
    }
    if let Some(l) = labels {
        env.push(("LEGION_WS_LABELS", l));
    }
    if let Some(r) = reviewer {
        env.push(("LEGION_WS_REVIEWER", r));
    }

    let output = call_plugin(&plugin_path, &["create-pr"], &env)?;
    let created: CreatedPR =
        serde_json::from_str(&output).map_err(|e| LegionError::WorkSource(e.to_string()))?;

    Ok(created)
}

/// Post a comment on an issue or PR via a work source plugin.
pub fn comment(plugin_name: &str, github_repo: &str, number: u64, body: &str) -> Result<()> {
    let plugin_path = find_plugin(plugin_name)
        .ok_or_else(|| LegionError::WorkSource(format!("plugin not found: {plugin_name}")))?;

    call_plugin(
        &plugin_path,
        &["comment"],
        &[
            ("LEGION_WS_REPO", github_repo),
            ("LEGION_WS_NUMBER", &number.to_string()),
            ("LEGION_WS_BODY", body),
        ],
    )?;

    Ok(())
}

/// A PR from an external tracker with review status.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExternalPR {
    pub number: u64,
    pub title: String,
    pub created_at: String,
    pub updated_at: String,
    pub head_ref_name: String,
    pub review_decision: Option<String>,
    pub is_draft: bool,
}

/// List open PRs from a work source plugin.
pub fn list_prs(plugin_name: &str, github_repo: &str) -> Result<Vec<ExternalPR>> {
    let plugin_path = match find_plugin(plugin_name) {
        Some(p) => p,
        None => return Ok(Vec::new()),
    };

    let output = call_plugin(
        &plugin_path,
        &["pr-list"],
        &[("LEGION_WS_REPO", github_repo)],
    )?;

    let prs: Vec<ExternalPR> =
        serde_json::from_str(&output).map_err(|e| LegionError::WorkSource(e.to_string()))?;

    Ok(prs)
}

/// A single CI check on a PR. `state` mirrors gh's check-state vocabulary;
/// see `gh pr checks --json state`. Stringly-typed so unknown states from a
/// future gh release deserialize cleanly instead of failing the whole call.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ExternalPRCheck {
    pub name: String,
    pub state: String,
    pub workflow: String,
    pub link: String,
    #[serde(default)]
    pub description: String,
}

/// States that `gh pr checks --json state` emits for a check that is
/// passing or still in flight. Anything NOT in this list is treated as
/// failing by `ExternalPRCheck::is_failing`. Single source of truth:
/// `is_failing` and its tests both reference this constant so "what is
/// passing?" and "what is failing?" cannot drift apart in review.
///
/// Adding a new passing state from a future gh release requires editing
/// exactly this list -- the correct review burden for a merge gate.
pub const PR_CHECK_PASSING_STATES: &[&str] = &[
    "SUCCESS",
    "NEUTRAL",
    "SKIPPED",
    "PENDING",
    "IN_PROGRESS",
    "QUEUED",
    "WAITING",
    "REQUESTED",
];

/// Concrete terminal failure states gh emits today. Kept as a separate
/// list (not the negation of `PR_CHECK_PASSING_STATES`) so the partition
/// test can assert that the two lists disjoint-cover the observed
/// state-space. Unknown states fall into neither and still count as
/// failing via `is_failing`'s fail-closed default.
///
/// Referenced only from the test that guards the partition; `#[allow(dead_code)]`
/// keeps it as a documented reference rather than hiding it in test code.
#[allow(dead_code)]
pub const PR_CHECK_FAILING_STATES: &[&str] = &[
    "FAILURE",
    "CANCELLED",
    "TIMED_OUT",
    "ACTION_REQUIRED",
    "STALE",
];

impl ExternalPRCheck {
    /// True unless `state` is a known passing or in-flight value
    /// (see `PR_CHECK_PASSING_STATES`).
    ///
    /// `legion pr checks` is a merge gate. Fail-closed on unknown states: a
    /// future gh release that adds a new failure variant must surface as a
    /// loud non-zero exit, not a silent green that lets a bad merge through.
    pub fn is_failing(&self) -> bool {
        !PR_CHECK_PASSING_STATES.contains(&self.state.as_str())
    }
}

/// Fetch CI check status for a PR via a work source plugin.
///
/// Returns `Err` when the named plugin is not installed. Callers gate merges
/// on this; an empty `Ok(Vec)` would render as a clean green and let a
/// misconfigured `watch.toml` auto-approve a PR with no checks ever run.
pub fn pr_checks(
    plugin_name: &str,
    github_repo: &str,
    pr_number: u64,
) -> Result<Vec<ExternalPRCheck>> {
    let plugin_path = find_plugin(plugin_name)
        .ok_or_else(|| LegionError::WorkSource(format!("plugin not found: {plugin_name}")))?;

    let pr_num_str = pr_number.to_string();
    let output = call_plugin(
        &plugin_path,
        &["pr-checks"],
        &[
            ("LEGION_WS_REPO", github_repo),
            ("LEGION_WS_PR_NUMBER", &pr_num_str),
        ],
    )?;

    let checks: Vec<ExternalPRCheck> =
        serde_json::from_str(&output).map_err(|e| LegionError::WorkSource(e.to_string()))?;

    Ok(checks)
}

/// Post a review on a PR via a work source plugin.
pub fn review_pr(
    plugin_name: &str,
    github_repo: &str,
    pr_number: u64,
    event: &str,
    body: Option<&str>,
    comments_file: Option<&str>,
) -> Result<()> {
    let plugin_path = find_plugin(plugin_name)
        .ok_or_else(|| LegionError::WorkSource(format!("plugin not found: {plugin_name}")))?;

    let pr_num_str = pr_number.to_string();
    let mut env: Vec<(&str, &str)> = vec![
        ("LEGION_WS_REPO", github_repo),
        ("LEGION_WS_PR_NUMBER", &pr_num_str),
        ("LEGION_WS_EVENT", event),
    ];
    if let Some(b) = body {
        env.push(("LEGION_WS_BODY", b));
    }
    if let Some(c) = comments_file {
        env.push(("LEGION_WS_COMMENTS", c));
    }

    call_plugin(&plugin_path, &["review"], &env)?;
    Ok(())
}

/// Merge a PR via a work source plugin. Refuses if not approved.
pub fn merge_pr(
    plugin_name: &str,
    github_repo: &str,
    pr_number: u64,
    strategy: &str,
    delete_branch: bool,
) -> Result<()> {
    let plugin_path = find_plugin(plugin_name)
        .ok_or_else(|| LegionError::WorkSource(format!("plugin not found: {plugin_name}")))?;

    let pr_num_str = pr_number.to_string();
    let delete_str = if delete_branch { "true" } else { "false" };
    let env: Vec<(&str, &str)> = vec![
        ("LEGION_WS_REPO", github_repo),
        ("LEGION_WS_PR_NUMBER", &pr_num_str),
        ("LEGION_WS_STRATEGY", strategy),
        ("LEGION_WS_DELETE_BRANCH", delete_str),
    ];

    call_plugin(&plugin_path, &["merge"], &env)?;
    Ok(())
}

/// Close a PR via a work source plugin without merging.
///
/// Passes the optional closing comment and branch-deletion flag through to the
/// plugin so agents can clean up stale or superseded PRs without touching `gh`
/// directly.
pub fn close_pr(
    plugin_name: &str,
    github_repo: &str,
    pr_number: u64,
    reason: Option<&str>,
    delete_branch: bool,
) -> Result<()> {
    let plugin_path = find_plugin(plugin_name)
        .ok_or_else(|| LegionError::WorkSource(format!("plugin not found: {plugin_name}")))?;

    let pr_num_str = pr_number.to_string();
    let delete_str = if delete_branch { "true" } else { "false" };
    let mut env: Vec<(&str, &str)> = vec![
        ("LEGION_WS_REPO", github_repo),
        ("LEGION_WS_PR_NUMBER", &pr_num_str),
        ("LEGION_WS_DELETE_BRANCH", delete_str),
    ];
    if let Some(r) = reason {
        env.push(("LEGION_WS_REASON", r));
    }

    call_plugin(&plugin_path, &["pr-close"], &env)?;
    Ok(())
}

/// Detect the external repo identifier from a workdir.
#[allow(dead_code)]
pub fn detect_repo(plugin_name: &str, workdir: &str) -> Result<Option<String>> {
    let plugin_path = match find_plugin(plugin_name) {
        Some(p) => p,
        None => return Ok(None),
    };

    let output = call_plugin(&plugin_path, &["detect"], &[("LEGION_WS_WORKDIR", workdir)])?;

    let trimmed = output.trim().to_string();
    if trimmed.is_empty() {
        Ok(None)
    } else {
        Ok(Some(trimmed))
    }
}

/// Sync issues from a work source into the kanban board.
///
/// Creates cards for issues that don't already have a linked card.
/// Returns the number of new cards created.
pub fn sync_issues(
    db: &Database,
    plugin_name: &str,
    source_repo: &str,
    workdir: &str,
    legion_repo: &str,
) -> Result<u64> {
    let issues = list_issues(plugin_name, source_repo, workdir)?;
    if issues.is_empty() {
        return Ok(0);
    }

    let existing_cards = kanban::board_cards(db)?;
    let existing_urls: HashSet<String> = existing_cards
        .iter()
        .filter_map(|c| c.source_url.as_ref())
        .cloned()
        .collect();

    let mut created = 0u64;
    for issue in &issues {
        if issue.url.is_empty() || existing_urls.contains(&issue.url) {
            continue;
        }

        let label_names: Vec<String> = issue
            .labels
            .iter()
            .filter_map(|l| {
                l.as_object()
                    .and_then(|obj| obj.get("name").and_then(|n| n.as_str()))
                    .or_else(|| l.as_str())
                    .map(String::from)
            })
            .collect();

        let labels = if label_names.is_empty() {
            None
        } else {
            Some(label_names.join(","))
        };

        let priority = if label_names.iter().any(|l| l == "critical") {
            "critical"
        } else if label_names.iter().any(|l| l == "high" || l == "priority") {
            "high"
        } else {
            "med"
        };

        kanban::create_card(
            db,
            legion_repo,
            legion_repo,
            &issue.title,
            issue.body.as_deref(),
            priority,
            labels.as_deref(),
            None,
            Some(&issue.url),
            Some(plugin_name),
            issue.created_at.as_deref(),
        )?;
        created += 1;
    }

    Ok(created)
}

/// Extract the issue number from a source URL.
pub fn extract_issue_number(url: &str) -> Option<u64> {
    url.rsplit('/').next().and_then(|s| s.parse().ok())
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

/// Full details of a PR, including body, head SHA, and mergeability.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExternalPRDetails {
    pub number: u64,
    pub title: String,
    pub state: String,
    pub author: String,
    pub created_at: String,
    pub updated_at: String,
    pub body: String,
    pub head_ref_name: String,
    pub head_sha: String,
    pub base_ref_name: String,
    pub is_draft: bool,
    pub review_decision: Option<String>,
    pub mergeable: Option<String>,
}

/// A single comment on a PR -- either a top-level conversation comment
/// or an inline review comment on a diff hunk. `kind` discriminates.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExternalPRComment {
    pub id: String,
    pub author: String,
    pub created_at: String,
    pub updated_at: String,
    pub body: String,
    pub kind: String,
    pub path: Option<String>,
    pub line: Option<u32>,
    pub in_reply_to_id: Option<String>,
}

impl ExternalPRComment {
    /// True when this comment is an inline review comment on a diff hunk
    /// (as opposed to a top-level issue-thread comment). Use this rather
    /// than `path.is_some()` at render sites -- the path-presence check
    /// couples the renderer to the current plugin shape and silently
    /// misroutes if a future plugin emits a review comment with no path.
    pub fn is_review(&self) -> bool {
        self.kind == "review"
    }
}

/// A submitted review with its body and any inline comments grouped under it.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExternalPRReview {
    pub id: String,
    pub author: String,
    pub state: String,
    pub submitted_at: Option<String>,
    pub body: String,
    pub comments: Vec<ExternalPRComment>,
}

/// Fetch a PR's body + metadata via a work source plugin.
///
/// Fail-closed: a missing plugin returns Err rather than an empty result.
/// Callers use this for human review of a PR thread; empty output would
/// be indistinguishable from an uncommented PR.
pub fn view_pr(plugin_name: &str, github_repo: &str, pr_number: u64) -> Result<ExternalPRDetails> {
    let plugin_path = find_plugin(plugin_name)
        .ok_or_else(|| LegionError::WorkSource(format!("plugin not found: {plugin_name}")))?;

    let pr_num_str = pr_number.to_string();
    let output = call_plugin(
        &plugin_path,
        &["view-pr"],
        &[
            ("LEGION_WS_REPO", github_repo),
            ("LEGION_WS_NUMBER", &pr_num_str),
        ],
    )?;

    serde_json::from_str(&output).map_err(|e| LegionError::WorkSource(e.to_string()))
}

/// List all comments on a PR (top-level + inline review comments) in
/// chronological order.
pub fn list_pr_comments(
    plugin_name: &str,
    github_repo: &str,
    pr_number: u64,
) -> Result<Vec<ExternalPRComment>> {
    let plugin_path = find_plugin(plugin_name)
        .ok_or_else(|| LegionError::WorkSource(format!("plugin not found: {plugin_name}")))?;

    let pr_num_str = pr_number.to_string();
    let output = call_plugin(
        &plugin_path,
        &["pr-comments"],
        &[
            ("LEGION_WS_REPO", github_repo),
            ("LEGION_WS_NUMBER", &pr_num_str),
        ],
    )?;

    serde_json::from_str(&output).map_err(|e| LegionError::WorkSource(e.to_string()))
}

/// List all submitted reviews with their bodies and inline comments.
pub fn list_pr_reviews(
    plugin_name: &str,
    github_repo: &str,
    pr_number: u64,
) -> Result<Vec<ExternalPRReview>> {
    let plugin_path = find_plugin(plugin_name)
        .ok_or_else(|| LegionError::WorkSource(format!("plugin not found: {plugin_name}")))?;

    let pr_num_str = pr_number.to_string();
    let output = call_plugin(
        &plugin_path,
        &["pr-reviews"],
        &[
            ("LEGION_WS_REPO", github_repo),
            ("LEGION_WS_NUMBER", &pr_num_str),
        ],
    )?;

    serde_json::from_str(&output).map_err(|e| LegionError::WorkSource(e.to_string()))
}

/// Fetch the raw CI log for a single job. Job IDs come from the numeric
/// suffix of `ExternalPRCheck::link` (`.../actions/runs/<run>/job/<job>`).
///
/// Returns the raw log bytes as a UTF-8 string. Callers are responsible
/// for rendering (ANSI codes, timestamp prefixes, etc. are preserved).
pub fn fetch_check_log(plugin_name: &str, github_repo: &str, job_id: u64) -> Result<String> {
    let plugin_path = find_plugin(plugin_name)
        .ok_or_else(|| LegionError::WorkSource(format!("plugin not found: {plugin_name}")))?;

    let job_id_str = job_id.to_string();
    call_plugin(
        &plugin_path,
        &["pr-check-log"],
        &[
            ("LEGION_WS_REPO", github_repo),
            ("LEGION_WS_JOB_ID", &job_id_str),
        ],
    )
}

/// Extract the numeric job id from a check's link. GitHub's check links
/// match `https://github.com/<owner>/<repo>/actions/runs/<run>/job/<job>`.
/// Returns None if the link does not match the expected pattern (custom
/// check runners, third-party integrations, etc.).
pub fn job_id_from_link(link: &str) -> Option<u64> {
    let suffix = link.rsplit_once("/job/")?.1;
    // Trim any query string or fragment.
    let numeric_part = suffix.split(|c: char| !c.is_ascii_digit()).next()?;
    numeric_part.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check_with_state(state: &str) -> ExternalPRCheck {
        ExternalPRCheck {
            name: "Test".into(),
            state: state.into(),
            workflow: "CI".into(),
            link: String::new(),
            description: String::new(),
        }
    }

    #[test]
    fn is_failing_terminal_failure_states() {
        for state in PR_CHECK_FAILING_STATES {
            assert!(
                check_with_state(state).is_failing(),
                "expected {state} to be failing"
            );
        }
    }

    #[test]
    fn is_failing_passing_and_in_flight_states() {
        for state in PR_CHECK_PASSING_STATES {
            assert!(
                !check_with_state(state).is_failing(),
                "expected {state} to NOT be failing"
            );
        }
    }

    /// The two declared state lists must disjoint-cover the observed
    /// state-space. If someone adds a state to both (or neither of) the
    /// lists, this test catches it before is_failing starts lying.
    /// Unknown states are fail-closed elsewhere
    /// (`is_failing_unknown_state_treated_as_failing`); this test guards
    /// only the states we have chosen to classify.
    #[test]
    fn pr_check_state_lists_partition() {
        let passing: std::collections::HashSet<&str> =
            PR_CHECK_PASSING_STATES.iter().copied().collect();
        let failing: std::collections::HashSet<&str> =
            PR_CHECK_FAILING_STATES.iter().copied().collect();

        let overlap: Vec<&&str> = passing.intersection(&failing).collect();
        assert!(
            overlap.is_empty(),
            "a state cannot be both passing and failing: {overlap:?}"
        );

        for state in &passing {
            assert!(!check_with_state(state).is_failing(), "{state}");
        }
        for state in &failing {
            assert!(check_with_state(state).is_failing(), "{state}");
        }
    }

    #[test]
    fn is_failing_unknown_state_treated_as_failing() {
        // Fail-closed: a future gh state we have not heard of must surface as
        // a loud non-zero exit, not a silent green. Adding a new passing
        // state requires an explicit allow-list edit -- the correct review
        // burden for a merge gate.
        assert!(check_with_state("FUTURE_GH_STATE").is_failing());
    }

    /// Regression guard for the silent-Ok hazard smugglr flagged on PR #291.
    ///
    /// `pr_checks` previously returned `Ok(Vec::new())` when the plugin
    /// binary could not be resolved. `legion pr checks` reads that empty
    /// vector, finds nothing failing, and exits 0 -- a false-green merge
    /// gate. Mirrors the close_issue / reopen_issue regression guards
    /// inherited from PR #227.
    #[test]
    fn pr_checks_returns_worksource_error_when_plugin_missing() {
        let result = pr_checks("nonexistent-plugin-xyz", "owner/repo", 1);
        assert!(
            matches!(result, Err(LegionError::WorkSource(_))),
            "expected WorkSource error, got {:?}",
            result
        );
    }

    #[test]
    fn extract_issue_number_from_url() {
        assert_eq!(
            extract_issue_number("https://github.com/runlegion/legion/issues/42"),
            Some(42)
        );
        assert_eq!(extract_issue_number("not-a-url"), None);
        assert_eq!(extract_issue_number(""), None);
    }

    #[test]
    fn find_plugin_returns_none_for_nonexistent() {
        let result = find_plugin("nonexistent-plugin-xyz");
        assert!(result.is_none());
    }

    #[test]
    fn close_pr_returns_worksource_error_when_plugin_missing() {
        // When the named plugin does not exist, close_pr must return a WorkSource
        // error, not panic. This exercises the find_plugin -> ok_or_else path.
        let result = close_pr("nonexistent-plugin-xyz", "owner/repo", 1, None, false);
        assert!(
            matches!(result, Err(LegionError::WorkSource(_))),
            "expected WorkSource error, got {:?}",
            result
        );
    }

    /// Regression guard for the PR #227 silent-fallthrough fix.
    ///
    /// A prior revision of `close_issue` had `None => return Ok(())` on the
    /// `find_plugin` branch, silently no-opping when the plugin was not
    /// installed. That made every caller believe the close had succeeded
    /// when in fact nothing had happened, and was the root cause of
    /// kanban-done-but-github-open drift observed in practice.
    ///
    /// If a future PR restores the silent fallthrough on `close_issue`,
    /// `reopen_issue`, or `edit_issue`, this test must fail.
    #[test]
    fn close_issue_returns_worksource_error_when_plugin_missing() {
        let result = close_issue("nonexistent-plugin-xyz", "owner/repo", 1, None);
        assert!(
            matches!(result, Err(LegionError::WorkSource(_))),
            "expected WorkSource error, got {:?}",
            result
        );
    }

    #[test]
    fn close_issue_returns_worksource_error_when_plugin_missing_with_comment() {
        // Same regression guard with a non-None comment, so a future change
        // that routes the comment through a different path and loses the
        // plugin check still fails this test.
        let result = close_issue(
            "nonexistent-plugin-xyz",
            "owner/repo",
            1,
            Some("closed by legion reconcile"),
        );
        assert!(
            matches!(result, Err(LegionError::WorkSource(_))),
            "expected WorkSource error, got {:?}",
            result
        );
    }

    #[test]
    fn reopen_issue_returns_worksource_error_when_plugin_missing() {
        let result = reopen_issue("nonexistent-plugin-xyz", "owner/repo", 1, None);
        assert!(
            matches!(result, Err(LegionError::WorkSource(_))),
            "expected WorkSource error, got {:?}",
            result
        );
    }

    #[test]
    fn edit_issue_returns_worksource_error_when_plugin_missing() {
        let result = edit_issue(
            "nonexistent-plugin-xyz",
            "owner/repo",
            1,
            Some("new title"),
            None,
        );
        assert!(
            matches!(result, Err(LegionError::WorkSource(_))),
            "expected WorkSource error, got {:?}",
            result
        );
    }

    /// Regression guard for the `edit_issue` empty-args defense-in-depth.
    ///
    /// The function rejects `title.is_none() && body.is_none()` before
    /// attempting any plugin call. The CLI handler in `main.rs` also
    /// performs the same check, but this guard is load-bearing for
    /// programmatic callers (propagation helpers, reconcile paths,
    /// future auto-propagation sites) that might bypass the CLI layer.
    /// If someone deletes the worksource check thinking the CLI check is
    /// sufficient, this test fails.
    #[test]
    fn edit_issue_rejects_empty_args_before_plugin_lookup() {
        // Uses a plugin name that would otherwise cause a plugin-lookup
        // failure. The assertion on the error message proves the
        // validation fired BEFORE the lookup rather than the lookup
        // happening to succeed with None args.
        let result = edit_issue("any-plugin-name", "owner/repo", 1, None, None);
        let Err(LegionError::WorkSource(msg)) = result else {
            panic!("expected WorkSource error, got {:?}", result);
        };
        assert!(
            msg.contains("at least one of title or body"),
            "expected validation error message, got: {msg}"
        );
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

    #[test]
    fn job_id_from_link_parses_github_actions_url() {
        let link = "https://github.com/runlegion/legion/actions/runs/24756870394/job/72431832342";
        assert_eq!(job_id_from_link(link), Some(72431832342));
    }

    #[test]
    fn job_id_from_link_tolerates_query_string() {
        let link = "https://github.com/foo/bar/actions/runs/1/job/99?pr=42";
        assert_eq!(job_id_from_link(link), Some(99));
    }

    #[test]
    fn job_id_from_link_returns_none_for_non_job_url() {
        // Third-party check integrations often link to a dashboard, not an
        // actions job. We must distinguish those from GitHub Actions jobs so
        // log fetch can be skipped with a clear marker rather than attempted
        // against the wrong endpoint.
        assert_eq!(job_id_from_link("https://example.com/checks/abc"), None);
        assert_eq!(job_id_from_link(""), None);
    }

    #[test]
    fn view_pr_returns_worksource_error_when_plugin_missing() {
        // Fail-closed mirror of pr_checks_returns_worksource_error_when_plugin_missing:
        // a missing plugin must not surface as "empty PR body" because a caller
        // (human reading via `legion pr view`) would read that as "the PR has
        // no description" rather than "we could not reach the API".
        let result = view_pr("nonexistent-plugin-xyz", "owner/repo", 1);
        assert!(
            matches!(result, Err(LegionError::WorkSource(_))),
            "expected WorkSource error, got {:?}",
            result
        );
    }

    #[test]
    fn list_pr_comments_returns_worksource_error_when_plugin_missing() {
        let result = list_pr_comments("nonexistent-plugin-xyz", "owner/repo", 1);
        assert!(
            matches!(result, Err(LegionError::WorkSource(_))),
            "expected WorkSource error, got {:?}",
            result
        );
    }

    #[test]
    fn list_pr_reviews_returns_worksource_error_when_plugin_missing() {
        let result = list_pr_reviews("nonexistent-plugin-xyz", "owner/repo", 1);
        assert!(
            matches!(result, Err(LegionError::WorkSource(_))),
            "expected WorkSource error, got {:?}",
            result
        );
    }

    #[test]
    fn fetch_check_log_returns_worksource_error_when_plugin_missing() {
        let result = fetch_check_log("nonexistent-plugin-xyz", "owner/repo", 42);
        assert!(
            matches!(result, Err(LegionError::WorkSource(_))),
            "expected WorkSource error, got {:?}",
            result
        );
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
