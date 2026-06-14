//! Kanban: the unit-of-work scheduler over the cards table.
//! Carved from a single kanban.rs (#615): `state` owns the
//! CardStatus/Priority/Action state machine, `format` owns rendering;
//! this file keeps the Card type, row mapping, and CRUD wrappers.

mod format;
pub mod reconcile;
mod state;

pub use format::*;
pub use state::*;

use std::str::FromStr;

use crate::db::Database;
use crate::error::{LegionError, Result};

/// A kanban card -- the unit of work in the scheduler.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Card {
    pub id: String,
    pub from_repo: String,
    pub to_repo: String,
    pub text: String,
    pub context: Option<String>,
    pub priority: Priority,
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
    pub problem: Option<String>,
    pub solution: Option<String>,
    pub acceptance: Option<String>,
    /// Optional bound document id (#528). When set, the spec document's
    /// `verification.acceptance` block overrides `tasks.acceptance` as the
    /// source of acceptance criteria for `legion verify`.
    pub document_id: Option<String>,
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
    let priority_str: String = row.get(5)?;
    let priority = Priority::from_str(&priority_str).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(5, rusqlite::types::Type::Text, Box::new(e))
    })?;
    Ok(Card {
        id: row.get(0)?,
        from_repo: row.get(1)?,
        to_repo: row.get(2)?,
        text: row.get(3)?,
        context: row.get(4)?,
        priority,
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
        problem: row.get(18)?,
        solution: row.get(19)?,
        acceptance: row.get(20)?,
        document_id: row.get(21)?,
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

/// Which slice of the board to return when listing cards.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum CardScope {
    /// Active work only: Pending, Accepted, NeedsInput, InReview, Blocked.
    /// Excludes Backlog (pre-consensus) and terminal (Done, Cancelled). This is
    /// the default for `kanban list` and the SessionStart "Current work" banner.
    WorkingSet,
    /// The raw, unconsented inbox: Backlog cards only.
    Backlog,
    /// Everything non-deleted, regardless of status.
    All,
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
///
/// When the card has a `document_id`, the bound document's `meta.status` and
/// hoisted `status` column are updated in the same transaction as the card's
/// status column. A bound document with an unparseable payload fails the whole
/// transition -- neither card nor spec drift silently (#528).
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
    db.transition_card_status_with_sync(
        id,
        &new_status.to_string(),
        note,
        ts,
        card.document_id.as_deref(),
        None,
    )?;
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
    priority: Priority,
    labels: Option<&str>,
    parent_card_id: Option<&str>,
    source_url: Option<&str>,
    source_type: Option<&str>,
    created_at: Option<&str>,
) -> Result<String> {
    // born-Backlog: every card is created in Backlog. Promotion to Pending is an
    // explicit transition (Action::Assign), never a side effect of creation.
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
        created_at,
        CardStatus::Backlog,
    )
}

/// List cards for a repo filtered by direction.
pub fn list_cards(
    db: &Database,
    repo: &str,
    direction: Direction,
    scope: CardScope,
) -> Result<Vec<Card>> {
    db.get_cards(repo, direction, scope)
}

/// Get all cards for the kanban board view.
pub fn board_cards(db: &Database) -> Result<Vec<Card>> {
    db.get_all_cards()
}

/// Get cards ready for a repo (pending status, used by surface).
pub fn get_ready_cards(db: &Database, repo: &str) -> Result<Vec<Card>> {
    db.get_pending_cards_for_repo(repo)
}

/// Get per-agent workload summary.
pub fn agent_workloads(db: &Database) -> Result<Vec<AgentWorkload>> {
    db.get_agent_workloads()
}

/// Get a single card by ID, returning `CardNotFound` if missing.
pub fn view_card(db: &Database, id: &str) -> Result<Card> {
    db.get_card_by_id(id)?
        .ok_or_else(|| LegionError::CardNotFound(id.to_string()))
}

/// Bind a document to a card (manual path; spec-gen uses the atomic insert).
///
/// Fails when the card is already bound, the document does not exist or is
/// archived, or another live card is already bound to the document.
pub fn bind_document(db: &Database, card_id: &str, document_id: &str) -> Result<()> {
    // Verify the document exists and is not archived before writing.
    let doc = db
        .get_document(document_id)?
        .ok_or_else(|| LegionError::WorkSource(format!("document '{document_id}' not found")))?;
    if doc.archived_at.is_some() {
        return Err(LegionError::WorkSource(format!(
            "document '{document_id}' is archived and cannot be bound"
        )));
    }
    db.bind_card_to_document(card_id, document_id)
}

/// Parameters for updating a card's mutable fields.
#[derive(Debug, Default)]
pub struct CardUpdateParams {
    /// New title text (replaces `text` column)
    pub text: Option<String>,
    /// New body -- re-parsed into problem/solution/acceptance
    pub body: Option<String>,
    /// New priority
    pub priority: Option<Priority>,
    /// Replacement label set (comma-separated)
    pub labels: Option<String>,
    /// Labels to append, deduplicated against existing
    pub add_labels: Option<String>,
    /// Labels to remove
    pub remove_labels: Option<String>,
}

/// Update mutable fields on a card.
///
/// Applies text, body (re-parsed), priority, and label changes, then
/// writes an audit entry. Returns the card id.
pub fn update_card(
    db: &Database,
    id: &str,
    _from_repo: &str,
    params: &CardUpdateParams,
) -> Result<String> {
    let card = db
        .get_card_by_id(id)?
        .ok_or_else(|| LegionError::CardNotFound(id.to_string()))?;

    // Resolve new labels from the three label params
    let new_labels: Option<String> = resolve_labels(
        card.labels.as_deref(),
        params.labels.as_deref(),
        params.add_labels.as_deref(),
        params.remove_labels.as_deref(),
    );

    // Parse body into structured fields if provided
    let (new_context, new_problem, new_solution, new_acceptance) =
        if let Some(ref body) = params.body {
            let parsed = crate::card_parse::parse_issue_body(body);
            let acceptance = parsed
                .acceptance
                .iter()
                .filter(|a| !a.is_empty())
                .cloned()
                .collect::<Vec<_>>();
            let acceptance_str = if acceptance.is_empty() {
                None
            } else {
                Some(acceptance.join("\n"))
            };
            (
                Some(body.as_str()),
                parsed.problem.as_deref().map(|s| s.to_string()),
                parsed.solution.as_deref().map(|s| s.to_string()),
                acceptance_str,
            )
        } else {
            (None, None, None, None)
        };

    let priority_str = params.priority.map(|p| p.to_string());
    db.update_card_fields(
        id,
        params.text.as_deref(),
        new_context,
        new_problem.as_deref(),
        new_solution.as_deref(),
        new_acceptance.as_deref(),
        priority_str.as_deref(),
        new_labels.as_deref(),
    )?;

    Ok(id.to_string())
}

/// Compute the final label string after applying add/remove/replace operations.
///
/// Priority: if `replace` is Some, use it verbatim. Otherwise start from
/// `existing`, add any `add_labels`, remove any `remove_labels`, deduplicate,
/// and return None if the result is empty.
fn resolve_labels(
    existing: Option<&str>,
    replace: Option<&str>,
    add_labels: Option<&str>,
    remove_labels: Option<&str>,
) -> Option<String> {
    if let Some(r) = replace {
        let cleaned = r
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join(",");
        return if cleaned.is_empty() {
            None
        } else {
            Some(cleaned)
        };
    }

    let mut set: Vec<String> = existing
        .unwrap_or("")
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();

    if let Some(add) = add_labels {
        for label in add.split(',').map(|s| s.trim().to_string()) {
            if !label.is_empty() && !set.contains(&label) {
                set.push(label);
            }
        }
    }

    if let Some(remove) = remove_labels {
        let to_remove: Vec<String> = remove
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        set.retain(|l| !to_remove.contains(l));
    }

    if set.is_empty() {
        None
    } else {
        Some(set.join(","))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::test_storage;

    // --- DB integration tests ---

    /// Create a card and Assign it to Pending -- the real born-Backlog -> consented
    /// flow. Returns the card id. Flow tests start from a ready (Pending) card.
    fn create_and_assign(
        db: &Database,
        from: &str,
        to: &str,
        text: &str,
        priority: Priority,
    ) -> String {
        let id = create_card(
            db, from, to, text, None, priority, None, None, None, None, None,
        )
        .expect("create");
        transition_card(db, &id, Action::Assign, None).expect("assign");
        id
    }

    #[test]
    fn create_and_list_cards() {
        let (db, _index, _dir) = test_storage();

        let id = create_card(
            &db,
            "kelex",
            "legion",
            "implement search",
            None,
            Priority::Med,
            None,
            None,
            None,
            None,
            None,
        )
        .expect("create");
        assert_eq!(id.len(), 36);

        let cards = list_cards(&db, "legion", Direction::Inbound, CardScope::All).expect("list");
        assert_eq!(cards.len(), 1);
        assert_eq!(cards[0].text, "implement search");
        // born-Backlog: a freshly created card lands in Backlog, not Pending.
        assert_eq!(cards[0].status, CardStatus::Backlog);
    }

    #[test]
    fn backlog_card_is_not_worked_until_assigned() {
        let (db, _index, _dir) = test_storage();

        let id = create_card(
            &db,
            "sean",
            "kelex",
            "unconsented",
            None,
            Priority::High,
            None,
            None,
            None,
            None,
            None,
        )
        .expect("create");

        // The born-Backlog safety property: an unconsented (Backlog) card must NOT
        // be handed to the agent as ready work. pick/peek filter on status=pending.
        assert!(
            peek_work(&db, "kelex").expect("peek").is_none(),
            "a Backlog card must not be picked up as ready work"
        );

        // An explicit Assign (operator consensus / planfile) promotes it to ready.
        transition_card(&db, &id, Action::Assign, None).expect("assign");
        assert!(
            peek_work(&db, "kelex").expect("peek").is_some(),
            "an assigned (Pending) card is ready work"
        );
    }

    #[test]
    fn list_cards_scopes_filter_by_status() {
        let (db, _index, _dir) = test_storage();

        // One card per relevant status. create_card yields Backlog; promote/transition
        // the rest. WorkingSet = Pending + Accepted here; Backlog and terminal excluded.
        let backlog_id = create_card(
            &db,
            "sean",
            "kelex",
            "raw inbox",
            None,
            Priority::Med,
            None,
            None,
            None,
            None,
            None,
        )
        .expect("create backlog");
        create_and_assign(&db, "sean", "kelex", "ready", Priority::Med); // -> Pending
        let accepted = create_and_assign(&db, "sean", "kelex", "in progress", Priority::Med);
        transition_card(&db, &accepted, Action::Accept, None).expect("accept");
        let done = create_and_assign(&db, "sean", "kelex", "shipped", Priority::Med);
        transition_card(&db, &done, Action::Accept, None).expect("accept");
        transition_card(&db, &done, Action::Done, None).expect("done");
        let cancelled = create_card(
            &db,
            "sean",
            "kelex",
            "scrapped",
            None,
            Priority::Med,
            None,
            None,
            None,
            None,
            None,
        )
        .expect("create cancelled");
        transition_card(&db, &cancelled, Action::Cancel, None).expect("cancel");

        // WorkingSet: only Pending + Accepted (Backlog, Done, Cancelled excluded).
        let ws = list_cards(&db, "kelex", Direction::Inbound, CardScope::WorkingSet).expect("ws");
        assert_eq!(ws.len(), 2, "working set should be Pending + Accepted only");
        assert!(
            ws.iter()
                .all(|c| matches!(c.status, CardStatus::Pending | CardStatus::Accepted)),
            "working set must exclude Backlog/Done/Cancelled"
        );

        // Backlog scope: only the raw inbox card.
        let bl = list_cards(&db, "kelex", Direction::Inbound, CardScope::Backlog).expect("bl");
        assert_eq!(bl.len(), 1);
        assert_eq!(bl[0].id, backlog_id);
        assert_eq!(bl[0].status, CardStatus::Backlog);

        // All: every non-deleted card regardless of status.
        let all = list_cards(&db, "kelex", Direction::Inbound, CardScope::All).expect("all");
        assert_eq!(all.len(), 5, "All should return every non-deleted card");
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
            Priority::High,
            Some("backend,search"),
            None,
            Some("https://github.com/runlegion/legion/issues/42"),
            Some("github"),
            None,
        )
        .expect("create");

        let card = db.get_card_by_id(&id).expect("get").expect("exists");
        assert_eq!(card.context.as_deref(), Some("related to issue #42"));
        assert_eq!(card.priority, Priority::High);
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

        let id = create_and_assign(&db, "kelex", "legion", "do the thing", Priority::Med);

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

        let id = create_and_assign(&db, "kelex", "legion", "blocked task", Priority::Med);

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

        create_and_assign(&db, "sean", "kelex", "low priority", Priority::Low);
        create_and_assign(&db, "sean", "kelex", "high priority", Priority::High);
        create_and_assign(&db, "sean", "kelex", "med priority", Priority::Med);

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

        create_and_assign(&db, "sean", "kelex", "peek test", Priority::Med);

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
            Priority::Med,
            None,
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
            Priority::Med,
            None,
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
    fn get_ready_cards_returns_assigned_pending_only() {
        let (db, _index, _dir) = test_storage();

        // Born-Backlog: an unassigned card is not ready work.
        create_card(
            &db,
            "kelex",
            "legion",
            "task one",
            None,
            Priority::Med,
            None,
            None,
            None,
            None,
            None,
        )
        .expect("create");
        let assigned = create_and_assign(&db, "kelex", "legion", "task two", Priority::High);

        let cards = get_ready_cards(&db, "legion").expect("get");
        assert_eq!(cards.len(), 1, "only the assigned (Pending) card is ready");
        assert_eq!(cards[0].id, assigned);
    }

    #[test]
    fn agent_workloads_summary() {
        let (db, _index, _dir) = test_storage();

        create_and_assign(&db, "sean", "kelex", "task 1", Priority::Med);
        create_and_assign(&db, "sean", "kelex", "task 2", Priority::High);
        create_and_assign(&db, "sean", "rafters", "task 3", Priority::Med);

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
            Priority::Med,
            None,
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
        let id = create_and_assign(&db, "sean", "kelex", "cancel test", Priority::Med);
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
        let id = create_and_assign(&db, "sean", "kelex", "block test", Priority::Med);
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

    #[test]
    fn created_at_override_preserves_source_timestamp() {
        let (db, _index, _dir) = test_storage();
        let source_date = "2026-04-03T10:00:00Z";
        let id = create_card(
            &db,
            "sean",
            "kelex",
            "old issue",
            None,
            Priority::Med,
            None,
            None,
            None,
            None,
            Some(source_date),
        )
        .expect("create");

        let card = db.get_card_by_id(&id).expect("get").expect("exists");
        assert_eq!(card.created_at, source_date);
    }

    #[test]
    fn created_at_override_affects_scheduling_order() {
        let (db, _index, _dir) = test_storage();

        // Create a "new" issue first (inserted first, but newer date)
        let new_id = create_card(
            &db,
            "sean",
            "kelex",
            "new issue",
            None,
            Priority::Med,
            None,
            None,
            None,
            None,
            Some("2026-04-07T00:00:00Z"),
        )
        .expect("create new");
        transition_card(&db, &new_id, Action::Assign, None).expect("assign new");

        // Create an "old" issue second (inserted second, but older date)
        let old_id = create_card(
            &db,
            "sean",
            "kelex",
            "old issue",
            None,
            Priority::Med,
            None,
            None,
            None,
            None,
            Some("2026-04-03T00:00:00Z"),
        )
        .expect("create old");
        transition_card(&db, &old_id, Action::Assign, None).expect("assign old");

        // Scheduler should pick the older issue first
        let card = peek_work(&db, "kelex").expect("peek").expect("has work");
        assert_eq!(card.text, "old issue");
    }

    #[test]
    fn synced_card_with_old_date_beats_manual_card() {
        let (db, _index, _dir) = test_storage();

        // Manual card created now (no override, gets Utc::now())
        let manual_id = create_card(
            &db,
            "sean",
            "kelex",
            "manual card",
            None,
            Priority::Med,
            None,
            None,
            None,
            None,
            None,
        )
        .expect("create manual");
        transition_card(&db, &manual_id, Action::Assign, None).expect("assign manual");

        // Synced card with an older GitHub creation date
        let synced_id = create_card(
            &db,
            "sean",
            "kelex",
            "old github issue",
            None,
            Priority::Med,
            None,
            None,
            None,
            None,
            Some("2026-03-01T00:00:00Z"),
        )
        .expect("create synced");
        transition_card(&db, &synced_id, Action::Assign, None).expect("assign synced");

        // Synced card's older date should win over manual card's recent date
        let card = peek_work(&db, "kelex").expect("peek").expect("has work");
        assert_eq!(card.text, "old github issue");
    }

    // --- view_card tests ---

    #[test]
    fn view_card_returns_card() {
        let (db, _index, _dir) = test_storage();
        let id = create_card(
            &db,
            "sean",
            "kelex",
            "view test",
            None,
            Priority::High,
            None,
            None,
            None,
            None,
            None,
        )
        .expect("create");
        let card = view_card(&db, &id).expect("view");
        assert_eq!(card.id, id);
        assert_eq!(card.text, "view test");
        assert_eq!(card.priority, Priority::High);
    }

    #[test]
    fn view_card_not_found_returns_error() {
        let (db, _index, _dir) = test_storage();
        let err = view_card(&db, "nonexistent-id").unwrap_err();
        assert!(matches!(err, LegionError::CardNotFound(_)));
    }

    // --- update_card tests ---

    #[test]
    fn update_card_title() {
        let (db, _index, _dir) = test_storage();
        let id = create_card(
            &db,
            "sean",
            "kelex",
            "original title",
            None,
            Priority::Med,
            None,
            None,
            None,
            None,
            None,
        )
        .expect("create");

        let params = CardUpdateParams {
            text: Some("updated title".to_string()),
            ..Default::default()
        };
        update_card(&db, &id, "sean", &params).expect("update");

        let card = view_card(&db, &id).expect("view");
        assert_eq!(card.text, "updated title");
    }

    #[test]
    fn update_card_body_reparses_structured_fields() {
        let (db, _index, _dir) = test_storage();
        let id = create_card(
            &db,
            "sean",
            "kelex",
            "card with body",
            None,
            Priority::Med,
            None,
            None,
            None,
            None,
            None,
        )
        .expect("create");

        let body = "## Problem\nThings break.\n## Solution\nFix it.\n## Acceptance criteria\n- Tests pass\n- Clippy clean";
        let params = CardUpdateParams {
            body: Some(body.to_string()),
            ..Default::default()
        };
        update_card(&db, &id, "sean", &params).expect("update");

        let card = view_card(&db, &id).expect("view");
        assert_eq!(card.problem.as_deref(), Some("Things break."));
        assert_eq!(card.solution.as_deref(), Some("Fix it."));
        assert!(card.acceptance.is_some());
        let acc = card.acceptance.unwrap();
        assert!(acc.contains("Tests pass"));
    }

    #[test]
    fn update_card_priority() {
        let (db, _index, _dir) = test_storage();
        let id = create_card(
            &db,
            "sean",
            "kelex",
            "priority test",
            None,
            Priority::Low,
            None,
            None,
            None,
            None,
            None,
        )
        .expect("create");

        let params = CardUpdateParams {
            priority: Some(Priority::Critical),
            ..Default::default()
        };
        update_card(&db, &id, "sean", &params).expect("update");

        let card = view_card(&db, &id).expect("view");
        assert_eq!(card.priority, Priority::Critical);
    }

    #[test]
    fn update_card_add_labels_deduplicates() {
        let (db, _index, _dir) = test_storage();
        let id = create_card(
            &db,
            "sean",
            "kelex",
            "labels test",
            None,
            Priority::Med,
            Some("backend,api"),
            None,
            None,
            None,
            None,
        )
        .expect("create");

        let params = CardUpdateParams {
            add_labels: Some("api,frontend".to_string()),
            ..Default::default()
        };
        update_card(&db, &id, "sean", &params).expect("update");

        let card = view_card(&db, &id).expect("view");
        let labels = card.labels.expect("labels");
        let parts: Vec<&str> = labels.split(',').collect();
        assert_eq!(
            parts.iter().filter(|&&l| l == "api").count(),
            1,
            "api deduplicated"
        );
        assert!(parts.contains(&"frontend"), "frontend added");
        assert!(parts.contains(&"backend"), "backend preserved");
    }

    #[test]
    fn update_card_remove_labels() {
        let (db, _index, _dir) = test_storage();
        let id = create_card(
            &db,
            "sean",
            "kelex",
            "remove labels test",
            None,
            Priority::Med,
            Some("backend,api,frontend"),
            None,
            None,
            None,
            None,
        )
        .expect("create");

        let params = CardUpdateParams {
            remove_labels: Some("api".to_string()),
            ..Default::default()
        };
        update_card(&db, &id, "sean", &params).expect("update");

        let card = view_card(&db, &id).expect("view");
        let labels = card.labels.expect("labels");
        assert!(!labels.split(',').any(|l| l == "api"), "api removed");
        assert!(
            labels.split(',').any(|l| l == "backend"),
            "backend preserved"
        );
    }

    #[test]
    fn update_card_replace_labels() {
        let (db, _index, _dir) = test_storage();
        let id = create_card(
            &db,
            "sean",
            "kelex",
            "replace labels test",
            None,
            Priority::Med,
            Some("backend,api"),
            None,
            None,
            None,
            None,
        )
        .expect("create");

        let params = CardUpdateParams {
            labels: Some("frontend,ux".to_string()),
            ..Default::default()
        };
        update_card(&db, &id, "sean", &params).expect("update");

        let card = view_card(&db, &id).expect("view");
        let labels = card.labels.expect("labels");
        assert_eq!(labels, "frontend,ux", "labels fully replaced");
    }

    #[test]
    fn update_card_not_found() {
        let (db, _index, _dir) = test_storage();
        let params = CardUpdateParams {
            text: Some("new title".to_string()),
            ..Default::default()
        };
        let err = update_card(&db, "nonexistent-id", "sean", &params).unwrap_err();
        assert!(matches!(err, LegionError::CardNotFound(_)));
    }

    #[test]
    fn update_card_sets_updated_at() {
        let (db, _index, _dir) = test_storage();
        let id = create_card(
            &db,
            "sean",
            "kelex",
            "timestamp test",
            None,
            Priority::Med,
            None,
            None,
            None,
            None,
            None,
        )
        .expect("create");
        let original = view_card(&db, &id).expect("view").updated_at;

        // Sleep a tick so the timestamp changes
        std::thread::sleep(std::time::Duration::from_millis(10));

        let params = CardUpdateParams {
            text: Some("changed".to_string()),
            ..Default::default()
        };
        update_card(&db, &id, "sean", &params).expect("update");
        let updated = view_card(&db, &id).expect("view").updated_at;
        assert!(updated >= original, "updated_at should advance");
    }

    // --- resolve_labels unit tests ---

    #[test]
    fn resolve_labels_replace_wins_over_existing() {
        let result = resolve_labels(Some("a,b"), Some("c,d"), None, None);
        assert_eq!(result.as_deref(), Some("c,d"));
    }

    #[test]
    fn resolve_labels_add_deduplicates() {
        let result = resolve_labels(Some("a,b"), None, Some("b,c"), None);
        let labels = result.expect("some");
        let parts: Vec<&str> = labels.split(',').collect();
        assert_eq!(parts.iter().filter(|&&l| l == "b").count(), 1);
        assert!(parts.contains(&"c"));
    }

    #[test]
    fn resolve_labels_remove_subsets() {
        let result = resolve_labels(Some("a,b,c"), None, None, Some("b"));
        let labels = result.expect("some");
        assert!(!labels.split(',').any(|l| l == "b"));
        assert!(labels.split(',').any(|l| l == "a"));
    }

    #[test]
    fn resolve_labels_empty_replace_returns_none() {
        let result = resolve_labels(Some("a,b"), Some(""), None, None);
        assert!(result.is_none());
    }
}
