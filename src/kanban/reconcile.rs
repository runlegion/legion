//! Kanban-to-worksource reconciliation (#444, #654).
//!
//! Detects and resolves drift between a local card's state and the lifecycle
//! of its linked GitHub issue. Two directions:
//!
//!   1. **stale-open**: the local card is terminal (`done`/`cancelled`) but
//!      the linked issue is still `OPEN`. Resolved by [`close_stale_open`],
//!      which closes the issue via the work source plugin.
//!   2. **shipped-pending**: the local card is active (`pending`, ...) but the
//!      linked issue is already `CLOSED`/`MERGED`. Resolved by
//!      [`cancel_shipped_pending`], which cancels the card locally without
//!      touching GitHub (the issue is already closed).
//!
//! [`scan_drift`] does both detections in a single board pass -- one
//! `view_issue` probe per card with a resolvable linked issue. Per-card
//! failures (no work source configured, probe error, unrecognised state) are
//! logged and skipped; they never abort the pass.
//!
//! The CLI `legion kanban reconcile` command and the daemon's periodic
//! auto-reconcile tick (#654) both build on these functions. The daemon only
//! ever runs the safe direction ([`cancel_shipped_pending`], purely local);
//! closing live GitHub issues from local state stays a manual, opt-in CLI
//! action behind `--close-stale`.

use super::{Action, Card, CardStatus, board_cards, transition_card};
use crate::db::{self, Database};
use crate::error::Result;
use crate::worksource;

/// A single drift finding: a card paired with the linked issue it diverged
/// from. Carries the resolved `source_repo` and issue `number` so the action
/// functions do not re-resolve work source config per card.
pub struct Drift {
    pub card: Card,
    pub number: u64,
    pub source_repo: String,
}

/// Both drift directions found in one board pass.
#[derive(Default)]
pub struct DriftReport {
    /// Terminal-local cards whose linked GitHub issue is still OPEN.
    pub stale_open: Vec<Drift>,
    /// Active-local cards whose linked GitHub issue is CLOSED/MERGED.
    pub shipped_pending: Vec<Drift>,
}

/// Scan the board once, partitioning cards by drift direction.
///
/// `repo_filter` restricts the scan to cards whose `to_repo` matches. Every
/// card with a `source_url`, an extractable issue number, a `source_type`,
/// and a configured work source triggers exactly one `view_issue` probe; its
/// live state decides which bucket (if any) it lands in. Cards missing any of
/// the first three are skipped silently (they are pure-local), while a card
/// whose `to_repo` has no configured work source logs a warning so an
/// operator can spot a misconfigured repo. A probe error or an unrecognised
/// issue state (anything outside the OPEN / CLOSED / MERGED set) is logged
/// and the card is skipped -- a single failing repo never aborts the pass.
///
/// `log_prefix` tags the skip/error breadcrumbs (`"[legion]"` for the CLI,
/// the loop prefix for the daemon).
pub fn scan_drift(
    db: &Database,
    repo_filter: Option<&str>,
    log_prefix: &str,
) -> Result<DriftReport> {
    let cards = board_cards(db)?;
    let mut report = DriftReport::default();

    for card in cards {
        if let Some(filter) = repo_filter
            && card.to_repo != *filter
        {
            continue;
        }

        // Terminal cards (done / cancelled) are direction-1 candidates;
        // anything still active (pending, in-progress, blocked, ...) is
        // direction-2.
        let is_terminal = matches!(card.status, CardStatus::Done | CardStatus::Cancelled);

        let Some(ref source_url) = card.source_url else {
            continue;
        };
        let Some(number) = worksource::extract_issue_number(source_url) else {
            continue;
        };
        let Some(source) = card.source_type.as_deref() else {
            continue;
        };
        let Some((_, source_repo, _)) = worksource::resolve_config(&card.to_repo) else {
            eprintln!(
                "{log_prefix} reconcile: skipping card {} -- to_repo '{}' has no work source configured",
                card.id, card.to_repo
            );
            continue;
        };

        // Query the live issue state. `view_issue` returns an `ExternalIssue`
        // whose `state` is "OPEN" or "CLOSED" for GitHub; a PR-merge reports
        // "CLOSED" (or "MERGED" on some plugins), so the closed-family covers
        // both shipped cases. Any other value is treated as unknown and
        // skipped rather than misclassified.
        let state = match worksource::view_issue(source, &source_repo, number) {
            Ok(issue) => issue.state,
            Err(e) => {
                eprintln!(
                    "{log_prefix} reconcile: failed to view {source_repo} issue #{number}: {e}"
                );
                continue;
            }
        };

        match (is_terminal, state.to_uppercase().as_str()) {
            (true, "OPEN") => report.stale_open.push(Drift {
                card,
                number,
                source_repo,
            }),
            (false, "CLOSED" | "MERGED") => report.shipped_pending.push(Drift {
                card,
                number,
                source_repo,
            }),
            _ => {}
        }
    }

    Ok(report)
}

/// Cancel shipped-pending cards locally, with no GitHub propagation -- the
/// linked issue is already CLOSED/MERGED, so closing it again would no-op or
/// double-touch the audit log. Writes one `cancel-card` audit row per card
/// (success or failure) so a reconcile that fails on some cards leaves a
/// durable record of which. Returns `(cancelled, failed)`. Per-card failures
/// are logged and counted; the pass never aborts.
pub fn cancel_shipped_pending(db: &Database, drifts: &[Drift], log_prefix: &str) -> (u64, u64) {
    let mut cancelled = 0_u64;
    let mut failed = 0_u64;

    for drift in drifts {
        let card = &drift.card;
        // The scan only enqueues cards whose source_type was Some;
        // defensively re-check so a future refactor cannot produce a
        // misleading audit row with an empty source_type.
        let Some(source_type) = card.source_type.as_deref() else {
            continue;
        };
        let note = format!(
            "reconcile: linked {}#{} already closed on GitHub",
            drift.source_repo, drift.number
        );
        match transition_card(db, &card.id, Action::Cancel, Some(&note)) {
            Ok(_) => {
                cancelled += 1;
                eprintln!(
                    "{log_prefix} reconcile: cancelled card {} ({}#{})",
                    card.id, drift.source_repo, drift.number
                );
                let details = serde_json::json!({
                    "card_id": card.id,
                    "propagation": "reconcile-shipped",
                    "external_number": drift.number,
                })
                .to_string();
                best_effort_audit(
                    db,
                    &db::AuditInput {
                        agent: &card.to_repo,
                        action: "cancel-card",
                        target_type: "card",
                        target_ref: &card.id,
                        task_id: Some(&card.id),
                        source_type,
                        details: Some(&details),
                        outcome: "success",
                    },
                );
            }
            Err(e) => {
                failed += 1;
                eprintln!(
                    "{log_prefix} reconcile: failed to cancel card {}: {e}",
                    card.id
                );
                let details = serde_json::json!({
                    "card_id": card.id,
                    "propagation": "reconcile-shipped",
                    "error": e.to_string(),
                })
                .to_string();
                best_effort_audit(
                    db,
                    &db::AuditInput {
                        agent: &card.to_repo,
                        action: "cancel-card",
                        target_type: "card",
                        target_ref: &card.id,
                        task_id: Some(&card.id),
                        source_type,
                        details: Some(&details),
                        outcome: "failure",
                    },
                );
            }
        }
    }

    (cancelled, failed)
}

/// Close the GitHub issues linked to stale-open cards (terminal-local but the
/// issue is still OPEN), posting a reconciling comment. Writes one
/// `close-issue` audit row per attempt. Returns `(closed, failed)`.
///
/// This is the destructive direction -- it mutates live GitHub state -- so
/// only the CLI invokes it, behind the explicit `--close-stale` flag. The
/// daemon's auto-reconcile never runs it.
pub fn close_stale_open(db: &Database, drifts: &[Drift], log_prefix: &str) -> (u64, u64) {
    let mut closed = 0_u64;
    let mut failed = 0_u64;

    for drift in drifts {
        let card = &drift.card;
        let Some(source) = card.source_type.as_deref() else {
            continue;
        };
        let comment = format!(
            "Closed by legion reconcile: local card {} is {} but this issue was left open. Reconciling to match the local state.",
            card.id,
            card.status.label()
        );
        match worksource::close_issue(source, &drift.source_repo, drift.number, Some(&comment)) {
            Ok(()) => {
                closed += 1;
                eprintln!(
                    "{log_prefix} reconcile: closed {} issue #{}",
                    drift.source_repo, drift.number
                );
                let details = serde_json::json!({
                    "card_id": card.id,
                    "propagation": "reconcile",
                })
                .to_string();
                best_effort_audit(
                    db,
                    &db::AuditInput {
                        agent: &card.to_repo,
                        action: "close-issue",
                        target_type: "issue",
                        target_ref: &drift.number.to_string(),
                        task_id: Some(&card.id),
                        source_type: source,
                        details: Some(&details),
                        outcome: "success",
                    },
                );
            }
            Err(e) => {
                failed += 1;
                eprintln!(
                    "{log_prefix} reconcile: failed to close {} issue #{}: {e}",
                    drift.source_repo, drift.number
                );
                let details = serde_json::json!({
                    "card_id": card.id,
                    "propagation": "reconcile",
                    "error": e.to_string(),
                })
                .to_string();
                best_effort_audit(
                    db,
                    &db::AuditInput {
                        agent: &card.to_repo,
                        action: "close-issue",
                        target_type: "issue",
                        target_ref: &drift.number.to_string(),
                        task_id: Some(&card.id),
                        source_type: source,
                        details: Some(&details),
                        outcome: "failure",
                    },
                );
            }
        }
    }

    (closed, failed)
}

/// Best-effort audit row: insert failures warn to stderr but never abort the
/// reconcile pass. Mirrors `cli::util::audit` without forcing the kanban
/// layer to depend on the cli layer.
fn best_effort_audit(db: &Database, input: &db::AuditInput<'_>) {
    if let Err(e) = db.insert_audit_entry(input) {
        eprintln!("[legion] warning: audit log failed: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kanban::{Priority, create_card};
    use crate::testutil::test_storage;

    /// Build a card with a linked GitHub issue and wrap it in a Drift so the
    /// action functions can be exercised without a live `view_issue` probe
    /// (which would need a real work source plugin).
    fn shipped_drift(db: &Database) -> Drift {
        let id = create_card(
            db,
            "legion",
            "legion",
            "reconcile test card",
            None,
            Priority::Med,
            None,
            None,
            Some("https://github.com/runlegion/legion/issues/999"),
            Some("github"),
            None,
        )
        .expect("create card");
        let card = db
            .get_card_by_id(&id)
            .expect("lookup")
            .expect("card exists");
        Drift {
            card,
            number: 999,
            source_repo: "runlegion/legion".to_string(),
        }
    }

    #[test]
    fn scan_drift_on_empty_board_is_empty() {
        let (db, _index, _dir) = test_storage();
        let report = scan_drift(&db, None, "[test]").expect("scan");
        assert!(report.stale_open.is_empty());
        assert!(report.shipped_pending.is_empty());
    }

    #[test]
    fn cancel_shipped_pending_empty_is_noop() {
        let (db, _index, _dir) = test_storage();
        assert_eq!(cancel_shipped_pending(&db, &[], "[test]"), (0, 0));
    }

    #[test]
    fn cancel_shipped_pending_cancels_card_and_audits() {
        let (db, _index, _dir) = test_storage();
        let drift = shipped_drift(&db);
        let card_id = drift.card.id.clone();

        let (cancelled, failed) =
            cancel_shipped_pending(&db, std::slice::from_ref(&drift), "[test]");
        assert_eq!((cancelled, failed), (1, 0));

        let card = db
            .get_card_by_id(&card_id)
            .expect("lookup")
            .expect("card exists");
        assert_eq!(card.status, CardStatus::Cancelled);

        // The cancel wrote a durable audit row so an operator can see what
        // reconcile touched.
        let entries = db
            .query_audit_log(None, Some("cancel-card"), 10)
            .expect("audit");
        assert!(
            entries
                .iter()
                .any(|e| e.target_ref == card_id && e.outcome == "success"),
            "expected a success cancel-card audit row for {card_id}, got {entries:?}"
        );
    }
}
