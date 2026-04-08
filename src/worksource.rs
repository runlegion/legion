use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::db::Database;
use crate::error::{LegionError, Result};
use crate::kanban;

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

/// Discover work source plugin paths.
/// Resolves a work source plugin from CLAUDE_PLUGIN_ROOT/worksources/.
/// Set by Claude Code in hook context, or exported by bin/legion wrapper.
fn find_plugin(name: &str) -> Option<PathBuf> {
    let plugin_root = std::env::var("CLAUDE_PLUGIN_ROOT").ok()?;
    let path = PathBuf::from(&plugin_root).join("worksources").join(name);
    if path.exists() {
        return Some(path);
    }
    None
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
pub fn close_issue(plugin_name: &str, github_repo: &str, number: u64) -> Result<()> {
    let plugin_path = match find_plugin(plugin_name) {
        Some(p) => p,
        None => return Ok(()),
    };

    call_plugin(
        &plugin_path,
        &["close", &number.to_string()],
        &[("LEGION_WS_REPO", github_repo)],
    )?;

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
}
