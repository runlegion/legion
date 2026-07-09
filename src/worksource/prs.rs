//! PR operations against a work source plugin (carved from
//! worksource.rs, #615).

use crate::error::Result;

use super::{decode_plugin_output, plugin_call, plugin_call_raw};

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

    plugin_call(plugin_name, &["create-pr"], &env)
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
///
/// Fail-closed: a missing plugin is an error, not an empty list (see
/// `require_plugin`).
pub fn list_prs(plugin_name: &str, github_repo: &str) -> Result<Vec<ExternalPR>> {
    plugin_call(
        plugin_name,
        &["pr-list"],
        &[("LEGION_WS_REPO", github_repo)],
    )
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

/// Result of resolving CI checks for a PR (#736). Carries the head SHA the
/// plugin actually queried alongside the checks themselves, so a caller can
/// tell "zero checks reported" apart from "zero checks for the commit we
/// pinned to" -- and can name that commit in the refusal message. The plugin
/// contract is to query `commits/{head_sha}/check-runs` for that exact SHA,
/// never a branch's latest suite (observed live on PR #735: the head had no
/// runs, but the branch's last-green parent commit's suite leaked through).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PrChecksResult {
    pub head_sha: String,
    pub checks: Vec<ExternalPRCheck>,
}

/// Fetch CI check status for a PR via a work source plugin, pinned to the
/// PR's head commit.
///
/// Returns `Err` when the named plugin is not installed. Callers gate merges
/// on this; an empty checks list is not silently treated as "nothing
/// failing" -- see `PrChecksResult` and the `no runs for head` refusal in
/// `legion pr checks` / `legion pr merge`.
pub fn pr_checks(plugin_name: &str, github_repo: &str, pr_number: u64) -> Result<PrChecksResult> {
    plugin_call(
        plugin_name,
        &["pr-checks"],
        &[
            ("LEGION_WS_REPO", github_repo),
            ("LEGION_WS_PR_NUMBER", &pr_number.to_string()),
        ],
    )
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

    plugin_call_raw(plugin_name, &["review"], &env)?;
    Ok(())
}

/// Outcome of a `merge_pr` call (#630). A base branch protected by a GitHub
/// merge queue cannot be merged directly -- the plugin enqueues the PR
/// instead and the queue completes the merge asynchronously. Callers must
/// not treat `queued` the same as an actual merge: kanban transitions and
/// issue-closing side effects that assume the PR is now merged would fire
/// too early against a PR that has only been queued, not merged.
///
/// `#[serde(default)]` on `queued`: a plugin predating #630 (or a
/// third-party one) that still emits the old `{"ok":true}` shape decodes
/// as `queued: false`, i.e. today's direct-merge behavior, rather than
/// failing the whole merge on a missing field. `merge_pr` itself handles
/// the other half of the old contract -- blank stdout -- since that is not
/// valid JSON `serde` can decode at all; see its doc comment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize)]
pub struct MergeOutcome {
    #[serde(default)]
    pub queued: bool,
}

/// Merge a PR via a work source plugin. Refuses if not approved.
///
/// Returns [`MergeOutcome`] so the caller can tell a direct merge apart from
/// an enqueue onto the base branch's merge queue (#630).
///
/// A plugin predating #630 (or a third-party one) may still honor the old
/// contract of empty stdout on success -- `merge` was listed among the
/// empty-output ops in `plugin_call_raw`'s doc comment before this change.
/// Blank stdout is not valid JSON, so decoding it would fail the whole
/// merge even though it actually succeeded; treat it as `queued: false`
/// (today's direct-merge behavior) instead.
pub fn merge_pr(
    plugin_name: &str,
    github_repo: &str,
    pr_number: u64,
    strategy: &str,
    delete_branch: bool,
) -> Result<MergeOutcome> {
    let pr_num_str = pr_number.to_string();
    let delete_str = if delete_branch { "true" } else { "false" };
    let env: Vec<(&str, &str)> = vec![
        ("LEGION_WS_REPO", github_repo),
        ("LEGION_WS_PR_NUMBER", &pr_num_str),
        ("LEGION_WS_STRATEGY", strategy),
        ("LEGION_WS_DELETE_BRANCH", delete_str),
    ];

    let output = plugin_call_raw(plugin_name, &["merge"], &env)?;
    if output.trim().is_empty() {
        return Ok(MergeOutcome { queued: false });
    }
    decode_plugin_output(&["merge"], &output)
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

    plugin_call_raw(plugin_name, &["pr-close"], &env)?;
    Ok(())
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
    plugin_call(
        plugin_name,
        &["view-pr"],
        &[
            ("LEGION_WS_REPO", github_repo),
            ("LEGION_WS_NUMBER", &pr_number.to_string()),
        ],
    )
}

/// List all comments on a PR (top-level + inline review comments) in
/// chronological order.
pub fn list_pr_comments(
    plugin_name: &str,
    github_repo: &str,
    pr_number: u64,
) -> Result<Vec<ExternalPRComment>> {
    plugin_call(
        plugin_name,
        &["pr-comments"],
        &[
            ("LEGION_WS_REPO", github_repo),
            ("LEGION_WS_NUMBER", &pr_number.to_string()),
        ],
    )
}

/// List all submitted reviews with their bodies and inline comments.
pub fn list_pr_reviews(
    plugin_name: &str,
    github_repo: &str,
    pr_number: u64,
) -> Result<Vec<ExternalPRReview>> {
    plugin_call(
        plugin_name,
        &["pr-reviews"],
        &[
            ("LEGION_WS_REPO", github_repo),
            ("LEGION_WS_NUMBER", &pr_number.to_string()),
        ],
    )
}

/// Fetch the raw CI log for a single job. Job IDs come from the numeric
/// suffix of `ExternalPRCheck::link` (`.../actions/runs/<run>/job/<job>`).
///
/// Returns the raw log bytes as a UTF-8 string. Callers are responsible
/// for rendering (ANSI codes, timestamp prefixes, etc. are preserved).
pub fn fetch_check_log(plugin_name: &str, github_repo: &str, job_id: u64) -> Result<String> {
    plugin_call_raw(
        plugin_name,
        &["pr-check-log"],
        &[
            ("LEGION_WS_REPO", github_repo),
            ("LEGION_WS_JOB_ID", &job_id.to_string()),
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
    use crate::error::LegionError;

    fn check_with_state(state: &str) -> ExternalPRCheck {
        ExternalPRCheck {
            name: "Test".into(),
            state: state.into(),
            workflow: "CI".into(),
            link: String::new(),
            description: String::new(),
        }
    }

    /// A plugin predating #630 (or a third-party one) that still emits the
    /// old empty-ish merge output must decode as `queued: false` -- today's
    /// direct-merge behavior -- rather than failing on a missing field.
    #[test]
    fn merge_outcome_defaults_queued_false_for_pre_630_plugin_output() {
        let outcome: MergeOutcome = serde_json::from_str(r#"{"ok":true}"#).unwrap();
        assert_eq!(outcome, MergeOutcome { queued: false });

        let outcome: MergeOutcome = serde_json::from_str("{}").unwrap();
        assert_eq!(outcome, MergeOutcome { queued: false });
    }

    #[test]
    fn merge_outcome_decodes_queued_true() {
        let outcome: MergeOutcome = serde_json::from_str(r#"{"queued":true}"#).unwrap();
        assert_eq!(outcome, MergeOutcome { queued: true });
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
    fn list_prs_returns_worksource_error_when_plugin_missing() {
        let result = list_prs("nonexistent-plugin-xyz", "owner/repo");
        assert!(
            matches!(result, Err(LegionError::WorkSource(_))),
            "expected WorkSource error, got {:?}",
            result
        );
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

    /// The plugin protocol wraps checks in `{"headSha": ..., "checks": [...]}`
    /// (#736) rather than a bare array, precisely so a zero-length `checks`
    /// list can still name the head commit it is zero-length FOR. Guards the
    /// camelCase wire shape against an accidental rename.
    #[test]
    fn pr_checks_result_deserializes_camel_case_wire_shape() {
        let json = r#"{"headSha":"ec3825b","checks":[{"name":"Tests","state":"SUCCESS","workflow":"CI","link":"https://x","description":""}]}"#;
        let parsed: PrChecksResult = serde_json::from_str(json).expect("valid PrChecksResult");
        assert_eq!(parsed.head_sha, "ec3825b");
        assert_eq!(parsed.checks.len(), 1);
        assert_eq!(parsed.checks[0].name, "Tests");
    }

    /// Zero check runs for the head SHA still deserializes cleanly -- an
    /// empty `checks` array with a populated `headSha` is the exact shape
    /// the plugin emits when the Checks API returns `total_count: 0` for
    /// that commit (#736).
    #[test]
    fn pr_checks_result_deserializes_zero_runs() {
        let json = r#"{"headSha":"ec3825b","checks":[]}"#;
        let parsed: PrChecksResult = serde_json::from_str(json).expect("valid PrChecksResult");
        assert_eq!(parsed.head_sha, "ec3825b");
        assert!(parsed.checks.is_empty());
    }
}
