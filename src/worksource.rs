use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Mutex, OnceLock};

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

/// Call a work source plugin with the given subcommand.
fn call_plugin(plugin_path: &Path, args: &[&str], env: &[(&str, &str)]) -> Result<String> {
    let mut cmd = Command::new(plugin_path);
    cmd.args(args);
    for (key, val) in env {
        cmd.env(key, val);
    }

    let output = cmd
        .output()
        .map_err(|e| LegionError::WorkSource(format!("failed to run plugin: {e}")))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(LegionError::WorkSource(format!("plugin failed: {stderr}")));
    }

    Ok(String::from_utf8_lossy(&output.stdout).to_string())
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
