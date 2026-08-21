//! Queue: `legion work`'s pick-next selection, now sourced live from
//! work-source issues (#931).
//!
//! #934 built an interim seam (`QueueItem`, `next_work`/`peek_work`) over
//! the local `tasks` table, deliberately independent of the card/`Card`
//! types so it would survive the card surface's eventual removal. #931 is
//! that removal, and its ruling resolves what the seam now reads FROM: the
//! card table duplicated what the work source (GitHub issues) already
//! owns, so once the card is gone the queue reads the work source
//! directly instead of a local cache -- no local `tasks` row is created,
//! synced, or claimed any more.
//!
//! This also means `peek_work` and `next_work` collapse into the same
//! operation. GitHub has no "accepted" concept, and reintroducing a local
//! claim ledger to fake one would recreate exactly the table this removal
//! deletes. The atomic-claim race safety `db::queue::pick_next_pending_work`
//! used to provide has no issue-side equivalent -- that is a deliberate
//! loss the ruling accepts, not an oversight. `legion done` (via
//! `cli::issue`'s shared close-and-verify path) is what marks work
//! finished, by closing the issue; there is no separate "complete" step
//! here.
//!
//! Persona agents (no work source configured in watch.toml) are out of
//! scope: `require_worksource` below fails loudly for them rather than
//! silently returning no work, matching #931's own premise that a
//! persona-specific queue store is a separate feature this removal must
//! not invent as a side effect.

use crate::error::Result;
use crate::worksource::{self, ExternalIssue};

/// A work item as the queue sees it. `id` is the issue URL -- used as the
/// opaque work-item id for `legion defer`/`legion undefer` and the
/// delegated-wake link (`db::wake::work_item_is_live` and friends), which
/// were always keyed on an opaque string, never a card row. Carries enough
/// content to display without a second lookup: unlike the #934 seam, there
/// is no local row left to look up against.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueueItem {
    pub id: String,
    pub number: u64,
    pub title: String,
    pub priority: String,
    pub created_at: String,
}

/// Derive a priority label from an issue's labels, mirroring the mapping
/// the old issue-to-card sync path used (`worksource::sync_issues`, #931
/// removed the sync but not the mapping it got right): a `critical` label
/// wins outright; `high` or `priority` is High; anything else is Med.
fn priority_from_labels(issue: &ExternalIssue) -> &'static str {
    let names: Vec<String> = issue
        .labels
        .iter()
        .filter_map(|l| {
            l.as_object()
                .and_then(|obj| obj.get("name").and_then(|n| n.as_str()))
                .or_else(|| l.as_str())
                .map(str::to_lowercase)
        })
        .collect();
    if names.iter().any(|l| l == "critical") {
        "critical"
    } else if names.iter().any(|l| l == "high" || l == "priority") {
        "high"
    } else {
        "med"
    }
}

/// Sort weight for a priority label -- lower sorts first. An unrecognized
/// label (should not happen; `priority_from_labels` only ever emits one of
/// these three) sorts as Med rather than panicking or erroring, since this
/// is display ordering, not a validated enum boundary.
fn priority_rank(p: &str) -> u8 {
    match p {
        "critical" => 0,
        "high" => 1,
        "med" => 2,
        "low" => 3,
        _ => 2,
    }
}

fn to_queue_item(issue: &ExternalIssue) -> QueueItem {
    QueueItem {
        id: issue.url.clone(),
        number: issue.number,
        title: issue.title.clone(),
        priority: priority_from_labels(issue).to_string(),
        created_at: issue.created_at.clone().unwrap_or_default(),
    }
}

/// Peek at the next candidate work item for a repo: the highest-priority
/// open issue on its configured work source, oldest-first within a
/// priority tier. Fails closed via `require_worksource` when `repo` has no
/// work source configured -- silence there would look identical to "no
/// work available" and hide a misconfiguration.
pub fn peek_work(repo: &str) -> Result<Option<QueueItem>> {
    let (plugin, source_repo, _workdir) = worksource::require_worksource(repo)?;
    let issues = worksource::list_all_issues(&plugin, &source_repo, "open", None)?;
    let mut items: Vec<QueueItem> = issues.iter().map(to_queue_item).collect();
    items.sort_by(|a, b| {
        priority_rank(&a.priority)
            .cmp(&priority_rank(&b.priority))
            .then_with(|| a.created_at.cmp(&b.created_at))
    });
    Ok(items.into_iter().next())
}

/// `legion work` without `--peek`. Identical to [`peek_work`] -- see the
/// module doc comment for why there is no separate claim step any more.
pub fn next_work(repo: &str) -> Result<Option<QueueItem>> {
    peek_work(repo)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn issue(number: u64, title: &str, labels: &[&str], created_at: &str) -> ExternalIssue {
        ExternalIssue {
            url: format!("https://github.com/owner/repo/issues/{number}"),
            number,
            title: title.to_string(),
            body: None,
            labels: labels
                .iter()
                .map(|l| serde_json::json!({"name": l}))
                .collect(),
            assignees: None,
            state: "OPEN".to_string(),
            created_at: Some(created_at.to_string()),
            updated_at: None,
        }
    }

    #[test]
    fn priority_from_labels_critical_wins() {
        let i = issue(1, "x", &["low", "critical"], "2026-01-01T00:00:00Z");
        assert_eq!(priority_from_labels(&i), "critical");
    }

    #[test]
    fn priority_from_labels_high_or_priority_label() {
        let i = issue(1, "x", &["high"], "2026-01-01T00:00:00Z");
        assert_eq!(priority_from_labels(&i), "high");
        let i2 = issue(1, "x", &["priority"], "2026-01-01T00:00:00Z");
        assert_eq!(priority_from_labels(&i2), "high");
    }

    #[test]
    fn priority_from_labels_defaults_to_med() {
        let i = issue(1, "x", &["bug"], "2026-01-01T00:00:00Z");
        assert_eq!(priority_from_labels(&i), "med");
        let none = issue(1, "x", &[], "2026-01-01T00:00:00Z");
        assert_eq!(priority_from_labels(&none), "med");
    }

    #[test]
    fn to_queue_item_uses_issue_url_as_id() {
        let i = issue(42, "fix the thing", &["high"], "2026-02-01T00:00:00Z");
        let item = to_queue_item(&i);
        assert_eq!(item.id, "https://github.com/owner/repo/issues/42");
        assert_eq!(item.number, 42);
        assert_eq!(item.title, "fix the thing");
        assert_eq!(item.priority, "high");
        assert_eq!(item.created_at, "2026-02-01T00:00:00Z");
    }

    #[test]
    fn priority_rank_orders_critical_first_low_last() {
        assert!(priority_rank("critical") < priority_rank("high"));
        assert!(priority_rank("high") < priority_rank("med"));
        assert!(priority_rank("med") < priority_rank("low"));
    }

    // `peek_work`/`next_work` themselves are not unit-tested here: both
    // resolve through `crate::data_dir()`, which caches its result in a
    // process-wide `OnceLock` (see `cli::datadir::data_dir`'s doc comment)
    // -- the first test in this binary to touch it wins for every test
    // that runs after, so a `LEGION_DATA_DIR`-dependent assertion here
    // would be order-dependent across the whole test binary, not just this
    // module. The fail-closed-on-unconfigured-repo behavior is exercised
    // at the integration level instead (`tests/integration/`), where each
    // test spawns a fresh `legion` subprocess with its own env.
}
