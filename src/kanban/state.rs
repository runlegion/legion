//! Card state machine: CardStatus, Priority, Action, and the legal
//! transition table (carved from kanban.rs, #615).

use std::fmt;
use std::str::FromStr;

use crate::error::{LegionError, Result};

/// Status of a kanban card.
///
/// Maps to human-friendly column names in the dashboard:
/// Backlog | Ready | In Progress | Needs Input | In Review | Blocked | Done | Cancelled
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CardStatus {
    Backlog,
    Pending,
    Accepted,
    NeedsInput,
    InReview,
    Blocked,
    /// Work handed off to a live watch-spawned wake attempt (#778). A
    /// sub-state of "in progress," not a terminal or human-blocked state:
    /// the agent that accepted the card is not the one doing the work right
    /// now, but the card is not abandoned either, because entry is gated on
    /// an unfakeable liveness signal (see `kanban::delegate_card`) and
    /// `tick_health` auto-reverts to `Accepted` the moment that signal goes
    /// dark. Named for the stance ("running elsewhere"), not "awaiting"
    /// (a snooze button that primes dormancy -- see reflection 019e98f4).
    Delegated,
    Done,
    Cancelled,
}

impl fmt::Display for CardStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Backlog => write!(f, "backlog"),
            Self::Pending => write!(f, "pending"),
            Self::Accepted => write!(f, "accepted"),
            Self::NeedsInput => write!(f, "needs-input"),
            Self::InReview => write!(f, "in-review"),
            Self::Blocked => write!(f, "blocked"),
            Self::Delegated => write!(f, "delegated"),
            Self::Done => write!(f, "done"),
            Self::Cancelled => write!(f, "cancelled"),
        }
    }
}

impl FromStr for CardStatus {
    type Err = LegionError;

    fn from_str(s: &str) -> Result<Self> {
        match s {
            "backlog" => Ok(Self::Backlog),
            "pending" => Ok(Self::Pending),
            "accepted" => Ok(Self::Accepted),
            "needs-input" => Ok(Self::NeedsInput),
            "in-review" => Ok(Self::InReview),
            "blocked" => Ok(Self::Blocked),
            "delegated" => Ok(Self::Delegated),
            "done" => Ok(Self::Done),
            "cancelled" => Ok(Self::Cancelled),
            other => Err(LegionError::InvalidCardStatus(other.to_string())),
        }
    }
}

impl CardStatus {
    /// Human-friendly column label for dashboard display.
    pub fn label(&self) -> &'static str {
        match self {
            Self::Backlog => "Backlog",
            Self::Pending => "Ready",
            Self::Accepted => "In Progress",
            Self::NeedsInput => "Needs Input",
            Self::InReview => "In Review",
            Self::Blocked => "Blocked",
            Self::Delegated => "Delegated",
            Self::Done => "Done",
            Self::Cancelled => "Cancelled",
        }
    }
}

/// Priority of a kanban card.
///
/// A closed set with the enum treatment `CardStatus` already got: a typo'd
/// priority is unrepresentable past the parse boundary, and the clap
/// surfaces derive their accepted values from this enum (`ValueEnum`).
/// The db-side scheduler ordering (`Database::PRIORITY_ORDER`) is a SQL
/// CASE over the same four literals; a test there asserts the SQL covers
/// every variant.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, clap::ValueEnum,
)]
#[serde(rename_all = "lowercase")]
pub enum Priority {
    Low,
    Med,
    High,
    Critical,
}

impl fmt::Display for Priority {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Low => write!(f, "low"),
            Self::Med => write!(f, "med"),
            Self::High => write!(f, "high"),
            Self::Critical => write!(f, "critical"),
        }
    }
}

impl FromStr for Priority {
    type Err = LegionError;

    fn from_str(s: &str) -> Result<Self> {
        match s {
            "low" => Ok(Self::Low),
            "med" => Ok(Self::Med),
            "high" => Ok(Self::High),
            "critical" => Ok(Self::Critical),
            other => Err(LegionError::InvalidPriority(other.to_string())),
        }
    }
}

impl Priority {
    /// Human-friendly label for dashboard display, mirroring
    /// `CardStatus::label`.
    pub fn label(&self) -> &'static str {
        match self {
            Self::Low => "Low",
            Self::Med => "Med",
            Self::High => "High",
            Self::Critical => "Critical",
        }
    }
}

/// Actions that trigger state transitions on a card.
#[derive(Debug, Clone, Copy)]
pub enum Action {
    /// backlog -> pending (assign to an agent)
    Assign,
    /// pending -> accepted (agent picks up work)
    Accept,
    /// accepted -> in-review
    Review,
    /// accepted -> needs-input (blocked on human)
    NeedInput,
    /// accepted -> needs-input (re-plan requested: the agent has concluded the
    /// frozen acceptance criteria are wrong, incomplete, or unachievable as
    /// written, and stops rather than improvising around them -- the
    /// spec-revision protocol, docs/decisions/2026-05-31-spec-revision-protocol.md).
    /// A distinct action from `NeedInput` (same destination status) so the
    /// transition history/audit trail can tell a re-plan request apart from a
    /// generic "needs human input".
    ReplanRequest,
    /// accepted -> blocked (technical blocker)
    Block,
    /// blocked -> accepted
    Unblock,
    /// accepted -> delegated (work handed off to a live watch-spawned wake
    /// attempt; #778). Mirrors Block/Unblock's shape -- entry is refused by
    /// the caller (`kanban::delegate_card`) unless a liveness predicate
    /// holds, so this transition itself stays unconditional at the FSM
    /// layer, same as Block never questions the reason.
    Delegate,
    /// delegated -> accepted. Fired either manually (the agent resumes the
    /// card itself) or programmatically by `tick_health`'s auto-revert sweep
    /// when the delegated attempt is no longer live.
    Undelegate,
    /// needs-input | in-review -> accepted
    Resume,
    /// accepted | in-review -> done
    Done,
    /// any except done -> cancelled
    Cancel,
    /// done | cancelled -> backlog
    Reopen,
}

/// Apply a state transition, returning the new status or an error.
pub fn transition(current: CardStatus, action: Action) -> Result<CardStatus> {
    let next = match (current, action) {
        (CardStatus::Backlog, Action::Assign) => CardStatus::Pending,
        (CardStatus::Pending, Action::Accept) => CardStatus::Accepted,
        (CardStatus::Accepted, Action::Review) => CardStatus::InReview,
        (CardStatus::Accepted, Action::NeedInput) => CardStatus::NeedsInput,
        (CardStatus::Accepted, Action::ReplanRequest) => CardStatus::NeedsInput,
        (CardStatus::Accepted, Action::Block) => CardStatus::Blocked,
        (CardStatus::Accepted, Action::Done) => CardStatus::Done,
        (CardStatus::Blocked, Action::Unblock) => CardStatus::Accepted,
        (CardStatus::Accepted, Action::Delegate) => CardStatus::Delegated,
        (CardStatus::Delegated, Action::Undelegate) => CardStatus::Accepted,
        (CardStatus::NeedsInput, Action::Resume) => CardStatus::Accepted,
        // Verify (#520) runs after review and can find an unprovable criterion;
        // routing the card from InReview to NeedsInput lets a human adjudicate
        // rather than rubber-stamping ->Done.
        (CardStatus::InReview, Action::NeedInput) => CardStatus::NeedsInput,
        (CardStatus::InReview, Action::Resume) => CardStatus::Accepted,
        (CardStatus::InReview, Action::Done) => CardStatus::Done,
        (CardStatus::Done, Action::Reopen) => CardStatus::Backlog,
        (CardStatus::Cancelled, Action::Reopen) => CardStatus::Backlog,
        // Cancel from any active state
        (s, Action::Cancel) if s != CardStatus::Done => CardStatus::Cancelled,
        (current, action) => {
            return Err(LegionError::InvalidCardTransition {
                action: format!("{action:?}").to_lowercase(),
                current: current.to_string(),
            });
        }
    };
    Ok(next)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_display_roundtrip() {
        for status in [
            CardStatus::Backlog,
            CardStatus::Pending,
            CardStatus::Accepted,
            CardStatus::NeedsInput,
            CardStatus::InReview,
            CardStatus::Blocked,
            CardStatus::Delegated,
            CardStatus::Done,
            CardStatus::Cancelled,
        ] {
            let s = status.to_string();
            let parsed = CardStatus::from_str(&s).expect("parse");
            assert_eq!(status, parsed);
        }
    }

    #[test]
    fn status_labels() {
        assert_eq!(CardStatus::Pending.label(), "Ready");
        assert_eq!(CardStatus::Accepted.label(), "In Progress");
        assert_eq!(CardStatus::NeedsInput.label(), "Needs Input");
        assert_eq!(CardStatus::Delegated.label(), "Delegated");
    }

    // --- State machine tests ---

    #[test]
    fn assign_from_backlog() {
        let result = transition(CardStatus::Backlog, Action::Assign);
        assert_eq!(result.expect("assign"), CardStatus::Pending);
    }

    #[test]
    fn accept_from_pending() {
        let result = transition(CardStatus::Pending, Action::Accept);
        assert_eq!(result.expect("accept"), CardStatus::Accepted);
    }

    #[test]
    fn review_from_accepted() {
        let result = transition(CardStatus::Accepted, Action::Review);
        assert_eq!(result.expect("review"), CardStatus::InReview);
    }

    #[test]
    fn need_input_from_accepted() {
        let result = transition(CardStatus::Accepted, Action::NeedInput);
        assert_eq!(result.expect("need-input"), CardStatus::NeedsInput);
    }

    #[test]
    fn block_from_accepted() {
        let result = transition(CardStatus::Accepted, Action::Block);
        assert_eq!(result.expect("block"), CardStatus::Blocked);
    }

    #[test]
    fn unblock_from_blocked() {
        let result = transition(CardStatus::Blocked, Action::Unblock);
        assert_eq!(result.expect("unblock"), CardStatus::Accepted);
    }

    // --- Delegated state (#778) ---

    #[test]
    fn delegate_from_accepted() {
        let result = transition(CardStatus::Accepted, Action::Delegate);
        assert_eq!(result.expect("delegate"), CardStatus::Delegated);
    }

    #[test]
    fn undelegate_from_delegated() {
        let result = transition(CardStatus::Delegated, Action::Undelegate);
        assert_eq!(result.expect("undelegate"), CardStatus::Accepted);
    }

    #[test]
    fn cannot_delegate_from_non_accepted_states() {
        for status in [
            CardStatus::Backlog,
            CardStatus::Pending,
            CardStatus::NeedsInput,
            CardStatus::InReview,
            CardStatus::Blocked,
            CardStatus::Delegated,
            CardStatus::Done,
            CardStatus::Cancelled,
        ] {
            let result = transition(status, Action::Delegate);
            assert!(
                result.is_err(),
                "delegate should be rejected from {status:?}"
            );
        }
    }

    #[test]
    fn cannot_undelegate_from_non_delegated_states() {
        for status in [
            CardStatus::Backlog,
            CardStatus::Pending,
            CardStatus::Accepted,
            CardStatus::NeedsInput,
            CardStatus::InReview,
            CardStatus::Blocked,
            CardStatus::Done,
            CardStatus::Cancelled,
        ] {
            let result = transition(status, Action::Undelegate);
            assert!(
                result.is_err(),
                "undelegate should be rejected from {status:?}"
            );
        }
    }

    #[test]
    fn cancel_from_delegated() {
        let result = transition(CardStatus::Delegated, Action::Cancel);
        assert_eq!(result.expect("cancel"), CardStatus::Cancelled);
    }

    #[test]
    fn resume_from_needs_input() {
        let result = transition(CardStatus::NeedsInput, Action::Resume);
        assert_eq!(result.expect("resume"), CardStatus::Accepted);
    }

    #[test]
    fn resume_from_in_review() {
        let result = transition(CardStatus::InReview, Action::Resume);
        assert_eq!(result.expect("resume"), CardStatus::Accepted);
    }

    #[test]
    fn done_from_accepted() {
        let result = transition(CardStatus::Accepted, Action::Done);
        assert_eq!(result.expect("done"), CardStatus::Done);
    }

    #[test]
    fn done_from_in_review() {
        let result = transition(CardStatus::InReview, Action::Done);
        assert_eq!(result.expect("done"), CardStatus::Done);
    }

    #[test]
    fn cancel_from_any_active() {
        for status in [
            CardStatus::Backlog,
            CardStatus::Pending,
            CardStatus::Accepted,
            CardStatus::NeedsInput,
            CardStatus::InReview,
            CardStatus::Blocked,
            CardStatus::Delegated,
        ] {
            let result = transition(status, Action::Cancel);
            assert_eq!(result.expect("cancel"), CardStatus::Cancelled);
        }
    }

    #[test]
    fn cannot_cancel_done() {
        let result = transition(CardStatus::Done, Action::Cancel);
        assert!(result.is_err());
    }

    #[test]
    fn reopen_from_done() {
        let result = transition(CardStatus::Done, Action::Reopen);
        assert_eq!(result.expect("reopen"), CardStatus::Backlog);
    }

    #[test]
    fn reopen_from_cancelled() {
        let result = transition(CardStatus::Cancelled, Action::Reopen);
        assert_eq!(result.expect("reopen"), CardStatus::Backlog);
    }

    #[test]
    fn cannot_accept_backlog() {
        let result = transition(CardStatus::Backlog, Action::Accept);
        assert!(result.is_err());
    }

    #[test]
    fn cannot_done_from_pending() {
        let result = transition(CardStatus::Pending, Action::Done);
        assert!(result.is_err());
    }

    #[test]
    fn cannot_block_pending() {
        let result = transition(CardStatus::Pending, Action::Block);
        assert!(result.is_err());
    }

    // --- Cancel idempotency ---

    #[test]
    fn cancel_from_cancelled_is_idempotent() {
        let result = transition(CardStatus::Cancelled, Action::Cancel);
        assert_eq!(result.expect("idempotent cancel"), CardStatus::Cancelled);
    }

    // --- Additional invalid transitions ---

    #[test]
    fn cannot_done_from_backlog() {
        let result = transition(CardStatus::Backlog, Action::Done);
        assert!(result.is_err());
    }

    #[test]
    fn cannot_assign_from_accepted() {
        let result = transition(CardStatus::Accepted, Action::Assign);
        assert!(result.is_err());
    }

    #[test]
    fn verify_routes_inreview_to_needs_input() {
        // #520: verify runs after review and can find an unprovable criterion.
        let result = transition(CardStatus::InReview, Action::NeedInput);
        assert_eq!(
            result.expect("InReview->NeedInput is valid"),
            CardStatus::NeedsInput
        );
    }

    #[test]
    fn cannot_review_from_pending() {
        let result = transition(CardStatus::Pending, Action::Review);
        assert!(result.is_err());
    }

    #[test]
    fn cannot_unblock_from_accepted() {
        let result = transition(CardStatus::Accepted, Action::Unblock);
        assert!(result.is_err());
    }

    #[test]
    fn cannot_reopen_from_accepted() {
        let result = transition(CardStatus::Accepted, Action::Reopen);
        assert!(result.is_err());
    }

    #[test]
    fn cannot_done_from_blocked() {
        let result = transition(CardStatus::Blocked, Action::Done);
        assert!(result.is_err());
    }

    // --- Spec-revision protocol (#554): re-plan request ---

    #[test]
    fn replan_request_from_accepted() {
        // The agent stops rather than improvising when the frozen AC turn
        // out wrong: Accepted -> NeedsInput, same destination as NeedInput
        // but a distinct action so the audit trail can tell them apart.
        let result = transition(CardStatus::Accepted, Action::ReplanRequest);
        assert_eq!(
            result.expect("replan-request is valid from accepted"),
            CardStatus::NeedsInput
        );
    }

    #[test]
    fn cannot_replan_request_from_non_accepted_states() {
        for status in [
            CardStatus::Backlog,
            CardStatus::Pending,
            CardStatus::NeedsInput,
            CardStatus::InReview,
            CardStatus::Blocked,
            CardStatus::Done,
            CardStatus::Cancelled,
        ] {
            let result = transition(status, Action::ReplanRequest);
            assert!(
                result.is_err(),
                "replan-request should be rejected from {status:?}"
            );
        }
    }
}
