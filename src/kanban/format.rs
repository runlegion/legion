//! Card rendering: list/view/JSON/goal-banner formatting
//! (carved from kanban.rs, #615).

use super::state::{CardStatus, Priority};
use super::{Card, Direction};

/// Build the board-derived goal banner (#525): the active Accepted card(s)
/// framed as the agent's completion condition, to carry across turns.
///
/// The native `/goal` cannot be set programmatically (no hook/flag/settings
/// surface as of CC v2.1.154), so this is the supported equivalent: the goal
/// is re-derived from the board on every SessionStart and printed on accept.
/// It "clears" with no separate state -- once nothing is Accepted (the card
/// reached a terminal or blocked status), this returns `None` and no goal is
/// shown. Returns `None` when no card is in progress.
pub fn format_active_goal(cards: &[Card]) -> Option<String> {
    let accepted: Vec<&Card> = cards
        .iter()
        .filter(|c| c.status == CardStatus::Accepted)
        .collect();
    if accepted.is_empty() {
        return None;
    }

    let mut out = String::from(
        "[Legion] Your goal -- the board's completion condition, carried across turns \
         (legion sets this from the card you accepted; you do not type /goal):",
    );
    for card in accepted {
        out.push_str(&format!("\n\n- {}  [card {}]", card.text.trim(), card.id));
        if let Some(ac) = card.acceptance.as_deref() {
            for item in ac.lines().map(str::trim).filter(|l| !l.is_empty()) {
                out.push_str(&format!("\n    - [ ] {item}"));
            }
        }
    }
    out.push_str(
        "\n\nKeep working until these acceptance criteria are met, then run verify. \
         The Stop gate holds you to it; \"should I keep going?\" is already answered -- yes.",
    );
    Some(out)
}

/// Format a priority tag for display. Empty for the default Med.
fn priority_tag(priority: Priority) -> String {
    if priority != Priority::Med {
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
        let prio = priority_tag(c.priority);
        let peer = match direction {
            Direction::Inbound => format!("from:{}", c.from_repo),
            Direction::Outbound => format!("to:{}", c.to_repo),
        };
        let note_part = c
            .note
            .as_deref()
            .map(|n| format!(" -- {}", n))
            .unwrap_or_default();
        let source_part = c
            .source_url
            .as_deref()
            .map(|u| format!(" <{}>", u))
            .unwrap_or_default();
        let date = crate::db::format_date(&c.created_at);
        output.push_str(&format!(
            "- [{}] {}{}{} ({}, {}{}) {}\n",
            c.status, c.text, prio, source_part, peer, date, note_part, c.id
        ));

        if let Some(summary) = crate::card_parse::card_summary(
            c.problem.as_deref(),
            c.acceptance.as_deref(),
            c.context.as_deref(),
        ) {
            output.push_str(&format!("  {}\n", summary));
        }
    }

    output
}

/// A compact summary of a card for JSON list output.
///
/// Contains only header fields -- excludes the full body to keep
/// `list --json` output machine-readable without copying large bodies.
#[derive(Debug, Clone, serde::Serialize)]
pub struct CardSummary {
    pub id: String,
    pub from_repo: String,
    pub to_repo: String,
    pub status: CardStatus,
    pub priority: Priority,
    pub title: String,
    pub created_at: String,
    pub updated_at: String,
    pub labels: Vec<String>,
    pub source_url: Option<String>,
}

/// Derive a display title from the card text field.
///
/// Takes the first non-blank line, strips a leading `## ` heading marker if
/// present, then truncates to 80 chars. This is the same derivation used by
/// `list --json`.
fn derive_title(text: &str) -> String {
    let first = text.lines().find(|l| !l.trim().is_empty()).unwrap_or(text);
    let stripped = first.strip_prefix("## ").unwrap_or(first).trim();
    crate::card_parse::truncate_chars(stripped, 80)
}

/// Convert a comma-separated labels string to a Vec.
fn labels_to_vec(labels: Option<&str>) -> Vec<String> {
    labels
        .unwrap_or("")
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

/// Format a single card for human-readable CLI display.
pub fn format_card_view(card: &Card) -> String {
    let mut out = String::new();
    out.push_str(&format!("Card: {}\n", card.id));
    out.push_str(&format!("Title: {}\n", derive_title(&card.text)));
    out.push_str(&format!("Status: {}\n", card.status.label()));
    out.push_str(&format!("Priority: {}\n", card.priority.label()));
    out.push_str(&format!(
        "From: {} -> To: {}\n",
        card.from_repo, card.to_repo
    ));
    out.push_str(&format!("Created: {}\n", card.created_at));
    out.push_str(&format!("Updated: {}\n", card.updated_at));

    if let Some(ref labels) = card.labels
        && !labels.is_empty()
    {
        out.push_str(&format!("Labels: {}\n", labels));
    }
    if let Some(ref url) = card.source_url {
        out.push_str(&format!("Source: {}\n", url));
    }

    // Structured sections -- prefer parsed columns, fall back to raw context
    let has_structured =
        card.problem.is_some() || card.solution.is_some() || card.acceptance.is_some();
    if has_structured {
        if let Some(ref p) = card.problem {
            out.push_str("\n## Problem\n");
            out.push_str(p);
            out.push('\n');
        }
        if let Some(ref s) = card.solution {
            out.push_str("\n## Solution\n");
            out.push_str(s);
            out.push('\n');
        }
        if let Some(ref a) = card.acceptance {
            out.push_str("\n## Acceptance\n");
            out.push_str(a);
            out.push('\n');
        }
    } else if let Some(ref ctx) = card.context {
        out.push_str("\n## Context\n");
        out.push_str(ctx);
        out.push('\n');
    }

    out
}

/// Serialize a card to a single JSON object for `view --json`.
pub fn format_card_json(card: &Card) -> crate::error::Result<String> {
    Ok(serde_json::to_string(card)?)
}

/// Convert a list of cards to JSONL (one summary object per line) for `list --json`.
pub fn format_card_list_json(cards: &[Card]) -> crate::error::Result<String> {
    let mut out = String::new();
    for card in cards {
        let summary = CardSummary {
            id: card.id.clone(),
            from_repo: card.from_repo.clone(),
            to_repo: card.to_repo.clone(),
            status: card.status,
            priority: card.priority,
            title: derive_title(&card.text),
            created_at: card.created_at.clone(),
            updated_at: card.updated_at.clone(),
            labels: labels_to_vec(card.labels.as_deref()),
            source_url: card.source_url.clone(),
        };
        out.push_str(&serde_json::to_string(&summary)?);
        out.push('\n');
    }
    Ok(out)
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
    use crate::kanban::*;
    use crate::testutil::test_storage;

    #[test]
    fn format_card_list_shows_cards() {
        let (db, _index, _dir) = test_storage();

        create_card(
            &db,
            "kelex",
            "legion",
            "test card",
            None,
            Priority::High,
            Some("backend"),
            None,
            None,
            None,
            None,
        )
        .expect("create");

        let cards = list_cards(&db, "legion", Direction::Inbound, CardScope::All).expect("list");
        let output = format_card_list(&cards, "legion", Direction::Inbound);
        assert!(output.contains("[Legion] Cards for legion"));
        assert!(output.contains("test card"));
        assert!(output.contains("[high]"));
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
            priority: Priority::High,
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
            problem: None,
            solution: None,
            acceptance: None,
            document_id: None,
            wake_at: None,
            pre_defer_status: None,
        };
        let output = format_work_card(&card);
        assert!(output.contains("Priority: high"));
        assert!(output.contains("From: sean"));
        assert!(output.contains("Labels: backend,search"));
        assert!(output.contains("Source: https://github.com"));
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
            priority: Priority::Med,
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
            problem: None,
            solution: None,
            acceptance: None,
            document_id: None,
            wake_at: None,
            pre_defer_status: None,
        };
        let output = format_work_card(&card);
        assert!(output.contains("minimal card"));
        assert!(!output.contains("Context:"));
        assert!(!output.contains("Labels:"));
        assert!(!output.contains("Source:"));
    }

    #[test]
    fn active_goal_derives_from_the_accepted_card() {
        let mk = |status: CardStatus, text: &str, ac: Option<&str>| Card {
            id: "card-1".to_string(),
            from_repo: "kelex".to_string(),
            to_repo: "kelex".to_string(),
            text: text.to_string(),
            context: None,
            priority: Priority::Med,
            status,
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
            problem: None,
            solution: None,
            acceptance: ac.map(str::to_string),
            document_id: None,
            wake_at: None,
            pre_defer_status: None,
        };

        // An Accepted card with AC becomes the goal, listing its criteria.
        let goal = format_active_goal(&[mk(
            CardStatus::Accepted,
            "ship the gate",
            Some("crit one\ncrit two"),
        )])
        .expect("an accepted card yields a goal");
        assert!(goal.contains("ship the gate"));
        assert!(goal.contains("crit one") && goal.contains("crit two"));
        assert!(goal.contains("card card-1"));

        // Nothing Accepted -> no goal (cleared by board state alone, AC #2).
        assert!(format_active_goal(&[mk(CardStatus::Pending, "not yet", Some("x"))]).is_none());
        assert!(format_active_goal(&[]).is_none());

        // Accepted but no AC -> still a goal, titled by the card text.
        let goal2 =
            format_active_goal(&[mk(CardStatus::Accepted, "chore card", None)]).expect("goal");
        assert!(goal2.contains("chore card"));
    }

    #[test]
    fn format_card_view_includes_structured_sections() {
        let card = Card {
            id: "test-id".to_string(),
            from_repo: "sean".to_string(),
            to_repo: "kelex".to_string(),
            text: "## Test Card".to_string(),
            context: None,
            priority: Priority::High,
            status: CardStatus::Pending,
            note: None,
            labels: Some("backend,api".to_string()),
            parent_card_id: None,
            source_url: Some("https://github.com/runlegion/legion/issues/1".to_string()),
            source_type: Some("github".to_string()),
            sort_order: 0,
            created_at: "2026-04-09T00:00:00Z".to_string(),
            updated_at: "2026-04-09T00:00:00Z".to_string(),
            assigned_at: None,
            started_at: None,
            completed_at: None,
            problem: Some("Things are broken".to_string()),
            solution: Some("Fix them".to_string()),
            acceptance: Some("Tests pass\nClipy clean".to_string()),
            document_id: None,
            wake_at: None,
            pre_defer_status: None,
        };
        let output = format_card_view(&card);
        assert!(output.contains("Test Card"), "title derived correctly");
        assert!(output.contains("## Problem"), "problem section present");
        assert!(output.contains("Things are broken"));
        assert!(output.contains("## Solution"), "solution section present");
        assert!(
            output.contains("## Acceptance"),
            "acceptance section present"
        );
        assert!(output.contains("Labels: backend,api"));
        assert!(output.contains("github.com"));
    }

    #[test]
    fn format_card_view_falls_back_to_context() {
        let card = Card {
            id: "test-id".to_string(),
            from_repo: "sean".to_string(),
            to_repo: "kelex".to_string(),
            text: "plain card".to_string(),
            context: Some("raw context text".to_string()),
            priority: Priority::Med,
            status: CardStatus::Pending,
            note: None,
            labels: None,
            parent_card_id: None,
            source_url: None,
            source_type: None,
            sort_order: 0,
            created_at: "2026-04-09T00:00:00Z".to_string(),
            updated_at: "2026-04-09T00:00:00Z".to_string(),
            assigned_at: None,
            started_at: None,
            completed_at: None,
            problem: None,
            solution: None,
            acceptance: None,
            document_id: None,
            wake_at: None,
            pre_defer_status: None,
        };
        let output = format_card_view(&card);
        assert!(output.contains("## Context"), "context section present");
        assert!(output.contains("raw context text"));
        assert!(!output.contains("## Problem"), "no problem section");
    }

    #[test]
    fn format_card_json_is_parseable() {
        let card = Card {
            id: "test-id".to_string(),
            from_repo: "sean".to_string(),
            to_repo: "kelex".to_string(),
            text: "json test".to_string(),
            context: None,
            priority: Priority::Med,
            status: CardStatus::Pending,
            note: None,
            labels: None,
            parent_card_id: None,
            source_url: None,
            source_type: None,
            sort_order: 0,
            created_at: "2026-04-09T00:00:00Z".to_string(),
            updated_at: "2026-04-09T00:00:00Z".to_string(),
            assigned_at: None,
            started_at: None,
            completed_at: None,
            problem: None,
            solution: None,
            acceptance: None,
            document_id: None,
            wake_at: None,
            pre_defer_status: None,
        };
        let json = format_card_json(&card).expect("json");
        let parsed: serde_json::Value = serde_json::from_str(&json).expect("parse");
        assert_eq!(parsed["id"].as_str().unwrap(), "test-id");
        assert_eq!(parsed["from_repo"].as_str().unwrap(), "sean");
    }

    // --- format_card_list_json tests ---

    #[test]
    fn format_card_list_json_emits_jsonl() {
        let (db, _index, _dir) = test_storage();
        create_card(
            &db,
            "sean",
            "kelex",
            "## Card One",
            None,
            Priority::High,
            Some("backend"),
            None,
            None,
            None,
            None,
        )
        .expect("create 1");
        create_card(
            &db,
            "sean",
            "kelex",
            "card two",
            None,
            Priority::Med,
            None,
            None,
            None,
            None,
            None,
        )
        .expect("create 2");

        let cards = list_cards(&db, "kelex", Direction::Inbound, CardScope::All).expect("list");
        let output = format_card_list_json(&cards).expect("json");
        let lines: Vec<&str> = output.lines().collect();
        assert_eq!(lines.len(), 2, "two lines for two cards");

        // Each line should be valid JSON with a title field
        let first: serde_json::Value = serde_json::from_str(lines[0]).expect("parse line 0");
        assert!(first["id"].is_string(), "id present");
        assert!(first["title"].is_string(), "title present");
        assert!(
            !first.as_object().unwrap().contains_key("context"),
            "no full body"
        );
        // title strips ## heading
        let any_card_one = lines.iter().any(|l| {
            serde_json::from_str::<serde_json::Value>(l)
                .ok()
                .and_then(|v| v["title"].as_str().map(|s| s.to_string()))
                .map(|t| t == "Card One")
                .unwrap_or(false)
        });
        assert!(any_card_one, "heading stripped from title");
    }

    #[test]
    fn format_card_list_json_labels_as_array() {
        let (db, _index, _dir) = test_storage();
        create_card(
            &db,
            "sean",
            "kelex",
            "labeled card",
            None,
            Priority::Med,
            Some("backend,api"),
            None,
            None,
            None,
            None,
        )
        .expect("create");

        let cards = list_cards(&db, "kelex", Direction::Inbound, CardScope::All).expect("list");
        let output = format_card_list_json(&cards).expect("json");
        let line = output.lines().next().expect("has output");
        let parsed: serde_json::Value = serde_json::from_str(line).expect("parse");
        let labels = parsed["labels"].as_array().expect("labels is array");
        assert_eq!(labels.len(), 2);
        assert!(labels.iter().any(|l| l.as_str() == Some("backend")));
        assert!(labels.iter().any(|l| l.as_str() == Some("api")));
    }
}
