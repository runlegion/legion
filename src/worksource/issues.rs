//! Issue operations against a work source plugin (carved from
//! worksource.rs, #615).

use std::collections::HashSet;
use crate::db::Database;
use crate::kanban;

use crate::error::{LegionError, Result};

use super::{plugin_call, plugin_call_raw};

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

/// List issues from a work source plugin.
///
/// Fail-closed: a missing plugin is an error, not an empty list. A sync
/// against a missing plugin must surface the misconfiguration rather than
/// report the tracker as empty.
pub fn list_issues(
    plugin_name: &str,
    github_repo: &str,
    workdir: &str,
) -> Result<Vec<ExternalIssue>> {
    plugin_call(
        plugin_name,
        &["list"],
        &[
            ("LEGION_WS_REPO", github_repo),
            ("LEGION_WS_WORKDIR", workdir),
        ],
    )
}

/// View a single issue via a work source plugin.
pub fn view_issue(plugin_name: &str, github_repo: &str, number: u64) -> Result<ExternalIssue> {
    plugin_call(
        plugin_name,
        &["view-issue"],
        &[
            ("LEGION_WS_REPO", github_repo),
            ("LEGION_WS_NUMBER", &number.to_string()),
        ],
    )
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
    let number_str = number.to_string();
    let mut env: Vec<(&str, &str)> = vec![
        ("LEGION_WS_REPO", github_repo),
        ("LEGION_WS_NUMBER", &number_str),
    ];
    if let Some(c) = comment {
        env.push(("LEGION_WS_COMMENT", c));
    }

    plugin_call_raw(plugin_name, &["close", &number_str], &env)?;

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
    let number_str = number.to_string();
    let mut env: Vec<(&str, &str)> = vec![
        ("LEGION_WS_REPO", github_repo),
        ("LEGION_WS_NUMBER", &number_str),
    ];
    if let Some(c) = comment {
        env.push(("LEGION_WS_COMMENT", c));
    }

    plugin_call_raw(plugin_name, &["reopen", &number_str], &env)?;

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

    plugin_call_raw(plugin_name, &["edit-issue"], &env)?;

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
    let parent_str = parent_number.to_string();
    let mut env: Vec<(&str, &str)> = vec![
        ("LEGION_WS_REPO", github_repo),
        ("LEGION_WS_PARENT_NUMBER", &parent_str),
        ("LEGION_WS_TITLE", title),
    ];
    if let Some(b) = body {
        env.push(("LEGION_WS_BODY", b));
    }
    plugin_call(plugin_name, &["create-sub-issue"], &env)
}

/// List sub-issues of a parent issue (#462). State filter is one of
/// `open` / `closed` / `all`; defaults to `open` when None.
///
/// Fail-closed: a missing plugin is an error, not an empty list (see
/// `require_plugin`).
pub fn list_sub_issues(
    plugin_name: &str,
    github_repo: &str,
    parent_number: u64,
    state: Option<&str>,
) -> Result<Vec<ExternalIssue>> {
    let parent_str = parent_number.to_string();
    let state_str = state.unwrap_or("open");
    let env: Vec<(&str, &str)> = vec![
        ("LEGION_WS_REPO", github_repo),
        ("LEGION_WS_PARENT_NUMBER", &parent_str),
        ("LEGION_WS_STATE", state_str),
    ];
    plugin_call(plugin_name, &["list-sub-issues"], &env)
}

pub fn create_issue(
    plugin_name: &str,
    github_repo: &str,
    title: &str,
    body: Option<&str>,
    labels: Option<&str>,
    assignee: Option<&str>,
) -> Result<CreatedIssue> {
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

    plugin_call(plugin_name, &["create-issue"], &env)
}

/// Post a comment on an issue or PR via a work source plugin.
pub fn comment(plugin_name: &str, github_repo: &str, number: u64, body: &str) -> Result<()> {
    plugin_call_raw(
        plugin_name,
        &["comment"],
        &[
            ("LEGION_WS_REPO", github_repo),
            ("LEGION_WS_NUMBER", &number.to_string()),
            ("LEGION_WS_BODY", body),
        ],
    )?;

    Ok(())
}

/// Sync issues from a work source into the kanban board.
///
/// Creates cards for issues that don't already have a linked card.
/// Returns the number of new cards created.
///
/// Fail-closed: a sync against a missing plugin is an error, not a
/// successful zero-issue sync (the #190 lesson, via `list_issues`).
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
        // Skip issues that already have a card. This skip is load-bearing for
        // born-Backlog: an already-imported card may have been promoted past
        // Backlog (Assign -> Pending -> ...) by operator consensus. Re-importing
        // it -- or turning this skip into an upsert -- would silently reset that
        // status back to Backlog and discard the consent. Do not change to an
        // upsert without preserving the existing card's status.
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
            kanban::Priority::Critical
        } else if label_names.iter().any(|l| l == "high" || l == "priority") {
            kanban::Priority::High
        } else {
            kanban::Priority::Med
        };

        // born-Backlog: synced issues are created in Backlog via create_card's
        // default. Do NOT introduce a status here -- promotion to Pending is an
        // explicit Assign (operator consensus / planfile), never a side effect of
        // import. Routing imports through create_card is what keeps AC #2 true.
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


#[cfg(test)]
mod tests {
    use super::*;

    /// The list ops were the last holdouts of the silent-empty policy:
    /// a missing plugin returned `Ok(Vec::new())`, so a sync against a
    /// missing plugin reported zero issues as success (the #190 shape).
    /// All three now fail closed through `require_plugin`.
    #[test]
    fn list_issues_returns_worksource_error_when_plugin_missing() {
        let result = list_issues("nonexistent-plugin-xyz", "owner/repo", "/tmp");
        assert!(
            matches!(result, Err(LegionError::WorkSource(_))),
            "expected WorkSource error, got {:?}",
            result
        );
    }

    #[test]
    fn list_sub_issues_returns_worksource_error_when_plugin_missing() {
        let result = list_sub_issues("nonexistent-plugin-xyz", "owner/repo", 1, None);
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

}
