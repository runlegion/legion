use std::fmt;
use std::str::FromStr;

use crate::db::Database;
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
            Self::Done => "Done",
            Self::Cancelled => "Cancelled",
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
    /// accepted -> blocked (technical blocker)
    Block,
    /// blocked -> accepted
    Unblock,
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
        (CardStatus::Accepted, Action::Block) => CardStatus::Blocked,
        (CardStatus::Accepted, Action::Done) => CardStatus::Done,
        (CardStatus::Blocked, Action::Unblock) => CardStatus::Accepted,
        (CardStatus::NeedsInput, Action::Resume) => CardStatus::Accepted,
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

/// A kanban card -- the unit of work in the scheduler.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Card {
    pub id: String,
    pub from_repo: String,
    pub to_repo: String,
    pub text: String,
    pub context: Option<String>,
    pub priority: String,
    pub status: CardStatus,
    pub note: Option<String>,
    pub labels: Option<String>,
    pub parent_card_id: Option<String>,
    pub source_url: Option<String>,
    pub source_type: Option<String>,
    pub sort_order: i32,
    pub created_at: String,
    pub updated_at: String,
    pub assigned_at: Option<String>,
    pub started_at: Option<String>,
    pub completed_at: Option<String>,
}

/// Per-agent workload summary for the dashboard agent strip.
#[derive(Debug, Clone, serde::Serialize)]
pub struct AgentWorkload {
    pub repo: String,
    pub active: u64,
    pub pending: u64,
    pub blocked: u64,
}

/// Map a database row to a Card struct.
pub fn map_card_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Card> {
    let status_str: String = row.get(6)?;
    let status = CardStatus::from_str(&status_str).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(6, rusqlite::types::Type::Text, Box::new(e))
    })?;
    Ok(Card {
        id: row.get(0)?,
        from_repo: row.get(1)?,
        to_repo: row.get(2)?,
        text: row.get(3)?,
        context: row.get(4)?,
        priority: row.get(5)?,
        status,
        note: row.get(7)?,
        labels: row.get(8)?,
        parent_card_id: row.get(9)?,
        source_url: row.get(10)?,
        source_type: row.get(11)?,
        sort_order: row.get::<_, Option<i32>>(12)?.unwrap_or(0),
        created_at: row.get(13)?,
        updated_at: row.get(14)?,
        assigned_at: row.get(15)?,
        started_at: row.get(16)?,
        completed_at: row.get(17)?,
    })
}

/// Direction filter for listing cards.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Direction {
    /// Cards assigned TO this repo.
    Inbound,
    /// Cards created BY this repo.
    Outbound,
}

/// Get the next pending card for a repo (the scheduler).
///
/// Selects the highest-priority unblocked card assigned to the repo
/// and atomically transitions it to accepted. Returns None if no
/// work is available.
pub fn next_work(db: &Database, repo: &str) -> Result<Option<Card>> {
    db.pick_next_card(repo)
}

/// Peek at the next pending card without accepting it.
///
/// Used by SessionStart hooks to show what's next without committing.
pub fn peek_work(db: &Database, repo: &str) -> Result<Option<Card>> {
    db.peek_next_card(repo)
}

/// Map an action to the timestamp field it should set.
fn timestamp_for_action(action: &Action) -> crate::db::CardTimestamp {
    match action {
        Action::Assign => crate::db::CardTimestamp::Assigned,
        Action::Accept => crate::db::CardTimestamp::Started,
        Action::Done | Action::Cancel => crate::db::CardTimestamp::Completed,
        _ => crate::db::CardTimestamp::None,
    }
}

/// Transition a card through the state machine.
pub fn transition_card(
    db: &Database,
    id: &str,
    action: Action,
    note: Option<&str>,
) -> Result<Card> {
    let card = db
        .get_card_by_id(id)?
        .ok_or_else(|| LegionError::CardNotFound(id.to_string()))?;

    let new_status = transition(card.status, action)?;
    let ts = timestamp_for_action(&action);
    db.update_card_status(id, &new_status.to_string(), note, ts)?;
    db.get_card_by_id(id)?
        .ok_or_else(|| LegionError::CardNotFound(id.to_string()))
}

/// Force-move a card to any status (bypasses state machine).
///
/// Used by the dashboard for drag-and-drop operations.
pub fn force_move(
    db: &Database,
    id: &str,
    new_status: CardStatus,
    sort_order: Option<i32>,
) -> Result<()> {
    db.force_move_card(id, &new_status.to_string(), sort_order)
}

/// Create a new card.
#[allow(clippy::too_many_arguments)]
pub fn create_card(
    db: &Database,
    from_repo: &str,
    to_repo: &str,
    text: &str,
    context: Option<&str>,
    priority: &str,
    labels: Option<&str>,
    parent_card_id: Option<&str>,
    source_url: Option<&str>,
    source_type: Option<&str>,
) -> Result<String> {
    db.insert_card(
        from_repo,
        to_repo,
        text,
        context,
        priority,
        labels,
        parent_card_id,
        source_url,
        source_type,
    )
}

/// List cards for a repo filtered by direction.
pub fn list_cards(db: &Database, repo: &str, direction: Direction) -> Result<Vec<Card>> {
    db.get_cards(repo, direction)
}

/// Get all cards for the kanban board view.
pub fn board_cards(db: &Database) -> Result<Vec<Card>> {
    db.get_all_cards()
}

/// Get cards ready for a repo (pending status, used by surface).
pub fn get_ready_cards(db: &Database, repo: &str) -> Result<Vec<Card>> {
    db.get_pending_cards_for_repo(repo)
}

/// Count ready cards for a repo (used by bullpen --count).
pub fn count_ready_cards(db: &Database, repo: &str) -> Result<u64> {
    db.count_pending_cards_for_repo(repo)
}

/// Get per-agent workload summary.
pub fn agent_workloads(db: &Database) -> Result<Vec<AgentWorkload>> {
    db.get_agent_workloads()
}

/// Format a priority tag for display.
fn priority_tag(priority: &str) -> String {
    if priority != "med" {
        format!(" [{}]", priority)
    } else {
        String::new()
    }
}

/// Format a card list for CLI display.
pub fn format_card_list(cards: &[Card], repo: &str, direction: Direction) -> String {
    if cards.is_empty() {
        return String::new();
    }

    let label = match direction {
        Direction::Inbound => "inbound",
        Direction::Outbound => "outbound",
    };

    let mut output = format!(
        "[Legion] Cards for {} ({}, {} total):\n",
        repo,
        label,
        cards.len()
    );

    for c in cards {
        let prio = priority_tag(&c.priority);
        let peer = match direction {
            Direction::Inbound => format!("from:{}", c.from_repo),
            Direction::Outbound => format!("to:{}", c.to_repo),
        };
        let note_part = c
            .note
            .as_deref()
            .map(|n| format!(" -- {}", n))
            .unwrap_or_default();
        let labels_part = c
            .labels
            .as_deref()
            .map(|l| format!(" {{{}}}", l))
            .unwrap_or_default();
        let source_part = c
            .source_url
            .as_deref()
            .map(|u| format!(" <{}>", u))
            .unwrap_or_default();
        let date = crate::db::format_date(&c.created_at);
        output.push_str(&format!(
            "- [{}] {}{}{}{} ({}, {}{}) {}\n",
            c.status, c.text, prio, labels_part, source_part, peer, date, note_part, c.id
        ));
    }

    output
}

/// Format ready cards for surface output.
pub fn format_ready_for_surface(cards: &[Card]) -> String {
    let mut output = String::new();
    for c in cards {
        let prio = priority_tag(&c.priority);
        let context_part = c
            .context
            .as_deref()
            .map(|ctx| {
                let truncated: String = ctx.chars().take(60).collect();
                let ellipsis = if ctx.chars().count() > 60 { "..." } else { "" };
                format!(" (context: {}{})", truncated, ellipsis)
            })
            .unwrap_or_default();
        let source_part = c
            .source_url
            .as_deref()
            .map(|u| format!(" <{}>", u))
            .unwrap_or_default();
        output.push_str(&format!(
            "- Card from {}: \"{}\"{}{}{}\n",
            c.from_repo, c.text, prio, context_part, source_part
        ));
    }
    output
}

/// Format a single card for the `legion work` output.
pub fn format_work_card(card: &Card) -> String {
    let mut output = format!("[Legion] Next task: {}\n", card.id);
    output.push_str(&format!("Priority: {}\n", card.priority));
    output.push_str(&format!("From: {}\n", card.from_repo));
    output.push_str(&format!("Description: {}\n", card.text));
    if let Some(ctx) = &card.context {
        output.push_str(&format!("Context: {}\n", ctx));
    }
    if let Some(labels) = &card.labels {
        output.push_str(&format!("Labels: {}\n", labels));
    }
    if let Some(url) = &card.source_url {
        output.push_str(&format!("Source: {}\n", url));
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::test_storage;

    #[test]
    fn status_display_roundtrip() {
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

    // --- DB integration tests ---

    #[test]
    fn create_and_list_cards() {
        let (db, _index, _dir) = test_storage();

        let id = create_card(
            &db,
            "kelex",
            "legion",
            "implement search",
            None,
            "med",
            None,
            None,
            None,
            None,
        )
        .expect("create");
        assert_eq!(id.len(), 36);

        let cards = list_cards(&db, "legion", Direction::Inbound).expect("list");
        assert_eq!(cards.len(), 1);
        assert_eq!(cards[0].text, "implement search");
        assert_eq!(cards[0].status, CardStatus::Pending);
    }

    #[test]
    fn create_with_all_fields() {
        let (db, _index, _dir) = test_storage();

        let id = create_card(
            &db,
            "kelex",
            "legion",
            "urgent task",
            Some("related to issue #42"),
            "high",
            Some("backend,search"),
            None,
            Some("https://github.com/runlegion/legion/issues/42"),
            Some("github"),
        )
        .expect("create");

        let card = db.get_card_by_id(&id).expect("get").expect("exists");
        assert_eq!(card.context.as_deref(), Some("related to issue #42"));
        assert_eq!(card.priority, "high");
        assert_eq!(card.labels.as_deref(), Some("backend,search"));
        assert_eq!(
            card.source_url.as_deref(),
            Some("https://github.com/runlegion/legion/issues/42")
        );
        assert_eq!(card.source_type.as_deref(), Some("github"));
    }

    #[test]
    fn full_lifecycle() {
        let (db, _index, _dir) = test_storage();

        let id = create_card(
            &db,
            "kelex",
            "legion",
            "do the thing",
            None,
            "med",
            None,
            None,
            None,
            None,
        )
        .expect("create");

        let card = transition_card(&db, &id, Action::Accept, None).expect("accept");
        assert_eq!(card.status, CardStatus::Accepted);
        assert!(card.started_at.is_some());

        let card = transition_card(&db, &id, Action::Done, Some("shipped")).expect("done");
        assert_eq!(card.status, CardStatus::Done);
        assert_eq!(card.note.as_deref(), Some("shipped"));
        assert!(card.completed_at.is_some());
    }

    #[test]
    fn block_unblock_flow() {
        let (db, _index, _dir) = test_storage();

        let id = create_card(
            &db,
            "kelex",
            "legion",
            "blocked task",
            None,
            "med",
            None,
            None,
            None,
            None,
        )
        .expect("create");

        transition_card(&db, &id, Action::Accept, None).expect("accept");
        let card =
            transition_card(&db, &id, Action::Block, Some("waiting on upstream")).expect("block");
        assert_eq!(card.status, CardStatus::Blocked);

        let card = transition_card(&db, &id, Action::Unblock, None).expect("unblock");
        assert_eq!(card.status, CardStatus::Accepted);
    }

    #[test]
    fn next_work_picks_highest_priority() {
        let (db, _index, _dir) = test_storage();

        create_card(
            &db,
            "sean",
            "kelex",
            "low priority",
            None,
            "low",
            None,
            None,
            None,
            None,
        )
        .expect("create low");
        create_card(
            &db,
            "sean",
            "kelex",
            "high priority",
            None,
            "high",
            None,
            None,
            None,
            None,
        )
        .expect("create high");
        create_card(
            &db,
            "sean",
            "kelex",
            "med priority",
            None,
            "med",
            None,
            None,
            None,
            None,
        )
        .expect("create med");

        let card = next_work(&db, "kelex").expect("work").expect("has work");
        assert_eq!(card.text, "high priority");
        assert_eq!(card.status, CardStatus::Accepted);
    }

    #[test]
    fn next_work_returns_none_when_empty() {
        let (db, _index, _dir) = test_storage();
        let card = next_work(&db, "kelex").expect("work");
        assert!(card.is_none());
    }

    #[test]
    fn peek_does_not_accept() {
        let (db, _index, _dir) = test_storage();

        create_card(
            &db,
            "sean",
            "kelex",
            "peek test",
            None,
            "med",
            None,
            None,
            None,
            None,
        )
        .expect("create");

        let card = peek_work(&db, "kelex").expect("peek").expect("has work");
        assert_eq!(card.status, CardStatus::Pending);

        // Card should still be pending
        let cards = get_ready_cards(&db, "kelex").expect("ready");
        assert_eq!(cards.len(), 1);
    }

    #[test]
    fn force_move_bypasses_state_machine() {
        let (db, _index, _dir) = test_storage();

        let id = create_card(
            &db,
            "sean",
            "kelex",
            "force move",
            None,
            "med",
            None,
            None,
            None,
            None,
        )
        .expect("create");

        // Force directly to done from pending (normally invalid)
        force_move(&db, &id, CardStatus::Done, None).expect("force");
        let card = db.get_card_by_id(&id).expect("get").expect("exists");
        assert_eq!(card.status, CardStatus::Done);
    }

    #[test]
    fn card_not_found() {
        let (db, _index, _dir) = test_storage();
        let err = transition_card(&db, "nonexistent-id", Action::Accept, None).unwrap_err();
        assert!(matches!(err, LegionError::CardNotFound(_)));
    }

    #[test]
    fn invalid_transition() {
        let (db, _index, _dir) = test_storage();

        let id = create_card(
            &db,
            "kelex",
            "legion",
            "premature",
            None,
            "med",
            None,
            None,
            None,
            None,
        )
        .expect("create");

        let err = transition_card(&db, &id, Action::Done, None).unwrap_err();
        assert!(matches!(err, LegionError::InvalidCardTransition { .. }));
    }

    #[test]
    fn count_and_get_ready_are_consistent() {
        let (db, _index, _dir) = test_storage();

        create_card(
            &db, "kelex", "legion", "task one", None, "med", None, None, None, None,
        )
        .expect("create");
        create_card(
            &db, "kelex", "legion", "task two", None, "high", None, None, None, None,
        )
        .expect("create");

        let count = count_ready_cards(&db, "legion").expect("count");
        let cards = get_ready_cards(&db, "legion").expect("get");
        assert_eq!(count, cards.len() as u64);
    }

    #[test]
    fn format_card_list_shows_cards() {
        let (db, _index, _dir) = test_storage();

        create_card(
            &db,
            "kelex",
            "legion",
            "test card",
            None,
            "high",
            Some("backend"),
            None,
            None,
            None,
        )
        .expect("create");

        let cards = list_cards(&db, "legion", Direction::Inbound).expect("list");
        let output = format_card_list(&cards, "legion", Direction::Inbound);
        assert!(output.contains("[Legion] Cards for legion"));
        assert!(output.contains("test card"));
        assert!(output.contains("[high]"));
        assert!(output.contains("{backend}"));
        assert!(output.contains("from:kelex"));
    }

    #[test]
    fn format_work_card_output() {
        let card = Card {
            id: "test-id".to_string(),
            from_repo: "sean".to_string(),
            to_repo: "kelex".to_string(),
            text: "implement search".to_string(),
            context: Some("see design doc".to_string()),
            priority: "high".to_string(),
            status: CardStatus::Accepted,
            note: None,
            labels: Some("backend,search".to_string()),
            parent_card_id: None,
            source_url: Some("https://github.com/runlegion/legion/issues/42".to_string()),
            source_type: Some("github".to_string()),
            sort_order: 0,
            created_at: "2026-04-03T00:00:00Z".to_string(),
            updated_at: "2026-04-03T00:00:00Z".to_string(),
            assigned_at: None,
            started_at: None,
            completed_at: None,
        };
        let output = format_work_card(&card);
        assert!(output.contains("Priority: high"));
        assert!(output.contains("From: sean"));
        assert!(output.contains("Labels: backend,search"));
        assert!(output.contains("Source: https://github.com"));
    }

    #[test]
    fn agent_workloads_summary() {
        let (db, _index, _dir) = test_storage();

        create_card(
            &db, "sean", "kelex", "task 1", None, "med", None, None, None, None,
        )
        .expect("create");
        create_card(
            &db, "sean", "kelex", "task 2", None, "high", None, None, None, None,
        )
        .expect("create");
        create_card(
            &db, "sean", "rafters", "task 3", None, "med", None, None, None, None,
        )
        .expect("create");

        let workloads = agent_workloads(&db).expect("workloads");
        assert!(workloads.len() >= 2);
        let kelex = workloads.iter().find(|w| w.repo == "kelex").expect("kelex");
        assert_eq!(kelex.pending, 2);
    }

    // --- Timestamp tracking tests ---

    #[test]
    fn assign_sets_assigned_at() {
        let (db, _index, _dir) = test_storage();
        // Create a backlog card (need to insert with backlog status)
        let id = create_card(
            &db,
            "sean",
            "kelex",
            "backlog item",
            None,
            "med",
            None,
            None,
            None,
            None,
        )
        .expect("create");
        // Force to backlog first
        force_move(&db, &id, CardStatus::Backlog, None).expect("backlog");

        let card = transition_card(&db, &id, Action::Assign, None).expect("assign");
        assert_eq!(card.status, CardStatus::Pending);
        assert!(
            card.assigned_at.is_some(),
            "assigned_at should be set on Assign"
        );
    }

    #[test]
    fn cancel_sets_completed_at() {
        let (db, _index, _dir) = test_storage();
        let id = create_card(
            &db,
            "sean",
            "kelex",
            "cancel test",
            None,
            "med",
            None,
            None,
            None,
            None,
        )
        .expect("create");
        transition_card(&db, &id, Action::Accept, None).expect("accept");
        let card = transition_card(&db, &id, Action::Cancel, None).expect("cancel");
        assert_eq!(card.status, CardStatus::Cancelled);
        assert!(
            card.completed_at.is_some(),
            "completed_at should be set on Cancel"
        );
    }

    #[test]
    fn block_unblock_does_not_clobber_started_at() {
        let (db, _index, _dir) = test_storage();
        let id = create_card(
            &db,
            "sean",
            "kelex",
            "block test",
            None,
            "med",
            None,
            None,
            None,
            None,
        )
        .expect("create");
        let card = transition_card(&db, &id, Action::Accept, None).expect("accept");
        let started = card.started_at.clone();
        assert!(started.is_some());

        transition_card(&db, &id, Action::Block, Some("blocker")).expect("block");
        let card = transition_card(&db, &id, Action::Unblock, None).expect("unblock");
        assert_eq!(
            card.started_at, started,
            "started_at should not change on block/unblock"
        );
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

    // --- Format edge cases ---

    #[test]
    fn format_card_list_empty() {
        let output = format_card_list(&[], "kelex", Direction::Inbound);
        assert!(output.is_empty());
    }

    #[test]
    fn format_work_card_minimal() {
        let card = Card {
            id: "test-id".to_string(),
            from_repo: "sean".to_string(),
            to_repo: "kelex".to_string(),
            text: "minimal card".to_string(),
            context: None,
            priority: "med".to_string(),
            status: CardStatus::Accepted,
            note: None,
            labels: None,
            parent_card_id: None,
            source_url: None,
            source_type: None,
            sort_order: 0,
            created_at: "2026-04-03T00:00:00Z".to_string(),
            updated_at: "2026-04-03T00:00:00Z".to_string(),
            assigned_at: None,
            started_at: None,
            completed_at: None,
        };
        let output = format_work_card(&card);
        assert!(output.contains("minimal card"));
        assert!(!output.contains("Context:"));
        assert!(!output.contains("Labels:"));
        assert!(!output.contains("Source:"));
    }
}
