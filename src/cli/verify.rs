//! `legion verify` and `legion quality-gate` handlers (carved from main.rs, #610).

use std::str::FromStr;

use clap::Subcommand;

use crate::cli::util::{git_head_commit_and_branch, open_db, read_file_or_stdin};
use crate::verify::GateResult;
use crate::{error, kanban, verify};

#[derive(Subcommand, Debug)]
pub(crate) enum QualityGateAction {
    /// Record a quality gate result for the current HEAD commit.
    ///
    /// Reads git HEAD and branch automatically. The skill runner calls this
    /// after inspecting the diff. `legion pr create` checks the gate before
    /// calling the work source so the result cannot be faked via a file flag.
    Record {
        /// Skill name (e.g., "legion-simplify")
        #[arg(long)]
        skill: String,

        /// Gate result: "clean" or "issues"
        #[arg(long, value_parser = ["clean", "issues"])]
        result: String,

        /// Number of findings (default 0)
        #[arg(long, default_value = "0")]
        findings_count: u64,

        /// Raw JSON details from the skill (full findings array)
        #[arg(long)]
        details_json: Option<String>,
    },
}

pub(crate) fn handle_quality_gate(action: QualityGateAction) -> error::Result<()> {
    match action {
        QualityGateAction::Record {
            skill,
            result,
            findings_count,
            details_json,
        } => {
            let gate_result = GateResult::from_str(&result)?;
            let (commit_hash, branch) = git_head_commit_and_branch()?;

            let database = open_db()?;
            let row = database.record_quality_gate(
                &branch,
                &commit_hash,
                &skill,
                gate_result,
                findings_count,
                details_json.as_deref(),
            )?;
            println!("{}", row.id);
        }
    }
    Ok(())
}

/// Resolve acceptance criteria for a card, with spec-document precedence (#528).
///
/// Returns `(criteria, source_label)`.
///
/// Precedence:
/// 1. When the card has a `document_id` AND the bound document's payload has a
///    non-empty `verification.acceptance` array, those strings become the AC.
///    Source label: `"spec:<document_id>"`.
/// 2. Otherwise: `tasks.acceptance` split by newline, same as before.
///    Source label: `"card"`.
fn resolve_acceptance_criteria(
    database: &crate::db::Database,
    card: &kanban::Card,
) -> error::Result<(Vec<String>, String)> {
    // When the card has a bound document, try to read verification.acceptance
    // from the document payload. A JSON parse failure is non-fatal: fall back
    // to tasks.acceptance so a corrupt payload does not hard-block verify.
    if let Some(ref doc_id) = card.document_id
        && let Some(doc) = database.get_document(doc_id)?
        && let Ok(value) = serde_json::from_str::<serde_json::Value>(&doc.payload)
        && let Some(arr) = value
            .get("verification")
            .and_then(|v| v.get("acceptance"))
            .and_then(|a| a.as_array())
    {
        let criteria: Vec<String> = arr
            .iter()
            .filter_map(|v| v.as_str())
            .map(str::to_owned)
            .filter(|s| !s.trim().is_empty())
            .collect();
        if !criteria.is_empty() {
            return Ok((criteria, format!("spec:{doc_id}")));
        }
    }
    // Fallback: tasks.acceptance.
    let criteria = verify::acceptance_items(card.acceptance.as_deref());
    Ok((criteria, "card".to_string()))
}

pub(crate) fn handle_verify(card: String, verdicts_file: Option<String>) -> error::Result<()> {
    let database = open_db()?;

    let card_row = database
        .get_card_by_id(&card)?
        .ok_or_else(|| error::LegionError::CardNotFound(card.clone()))?;

    // AC source precedence (#528):
    // 1. When the card has a bound document AND the document's payload has a
    //    non-empty `verification.acceptance` array, those strings are the
    //    canonical criteria -- the spec is authoritative.
    // 2. Otherwise fall back to `tasks.acceptance` exactly as before.
    let (acceptance, ac_source) = resolve_acceptance_criteria(&database, &card_row)?;

    // Read the agent's per-criterion verdicts (file or stdin).
    let raw = read_file_or_stdin(verdicts_file.as_deref(), "--verdicts-file")?;
    let results: Vec<verify::AcResult> = serde_json::from_str(&raw).map_err(|e| {
        error::LegionError::WorkSource(format!(
            "failed to parse verdicts JSON (expected a list of \
             {{criterion, verdict, evidence}}): {e}"
        ))
    })?;

    let decision = verify::decide(&acceptance, &results);

    // Record the verdict as a card-keyed gate so `legion done` can gate
    // on it regardless of which commit it runs on (e.g. post-merge).
    let skill = verify::verify_gate_key(&card);
    let (commit_hash, branch) = git_head_commit_and_branch()?;
    let details = serde_json::json!({
        "skill": "legion-verify",
        "card": card,
        "decision": format!("{decision:?}"),
        "results": results,
    })
    .to_string();
    let findings = match &decision {
        verify::VerifyDecision::Block { failed } => failed.len() as u64,
        verify::VerifyDecision::NeedsInput { uncertain } => uncertain.len() as u64,
        verify::VerifyDecision::Incomplete { unaddressed } => *unaddressed as u64,
        verify::VerifyDecision::NoCheckableAc => 1,
        verify::VerifyDecision::Proceed => 0,
    };
    let gate_result = if decision.allows_done() {
        GateResult::Clean
    } else {
        GateResult::Issues
    };
    database.record_quality_gate(
        &branch,
        &commit_hash,
        &skill,
        gate_result,
        findings,
        Some(&details),
    )?;

    match decision {
        verify::VerifyDecision::Proceed => {
            println!(
                "[legion] verify PASS for card {card} ({} criteria, source: {ac_source}). ->Done is unblocked.",
                acceptance.len()
            );
        }
        verify::VerifyDecision::NoCheckableAc => {
            eprintln!(
                "[legion] verify BLOCKED for card {card}: no acceptance criteria to check. \
                 A card cannot reach Done without checkable criteria -- add them upstream."
            );
            return Err(error::LegionError::ExitWith(1));
        }
        verify::VerifyDecision::Incomplete { unaddressed } => {
            eprintln!(
                "[legion] verify BLOCKED for card {card}: {unaddressed} of {} criteria have \
                 no verdict. Emit one verdict per criterion.",
                acceptance.len()
            );
            return Err(error::LegionError::ExitWith(1));
        }
        verify::VerifyDecision::Block { failed } => {
            eprintln!(
                "[legion] verify FAIL for card {card} -- {} criterion(s) not satisfied:",
                failed.len()
            );
            for c in &failed {
                eprintln!("  - {c}");
            }
            eprintln!("\n->Done is blocked. Finish the work and re-verify.");
            return Err(error::LegionError::ExitWith(1));
        }
        verify::VerifyDecision::NeedsInput { uncertain } => {
            eprintln!(
                "[legion] verify UNCERTAIN for card {card} -- {} criterion(s) cannot be \
                 mechanically confirmed:",
                uncertain.len()
            );
            for c in &uncertain {
                eprintln!("  - {c}");
            }
            // Route to a human rather than rubber-stamp ->Done. The gate
            // is already recorded non-clean, so ->Done stays blocked even
            // if the card is not in a state this transition accepts.
            match kanban::transition_card(
                &database,
                &card,
                kanban::Action::NeedInput,
                Some("verify: unprovable acceptance criteria, needs human adjudication"),
            ) {
                Ok(_) => {
                    eprintln!("\nCard routed to NeedsInput. ->Done stays blocked until resolved.")
                }
                Err(e) => eprintln!(
                    "\n->Done stays blocked. (Could not auto-move card to NeedsInput: \
                     {e}; move it manually.)"
                ),
            }
            return Err(error::LegionError::ExitWith(1));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::testutil::test_db;
    use crate::documents::DocumentMeta;
    use crate::kanban::{Card, CardStatus, Priority};

    fn make_card(doc_id: Option<&str>, acceptance: Option<&str>) -> Card {
        Card {
            id: "card-test".to_string(),
            from_repo: "legion".to_string(),
            to_repo: "legion".to_string(),
            text: "test card".to_string(),
            context: None,
            priority: Priority::Med,
            status: CardStatus::Accepted,
            note: None,
            labels: None,
            parent_card_id: None,
            source_url: None,
            source_type: None,
            sort_order: 0,
            created_at: "2026-06-12T00:00:00Z".to_string(),
            updated_at: "2026-06-12T00:00:00Z".to_string(),
            assigned_at: None,
            started_at: None,
            completed_at: None,
            problem: None,
            solution: None,
            acceptance: acceptance.map(str::to_string),
            document_id: doc_id.map(str::to_string),
        }
    }

    /// When no document_id, falls back to tasks.acceptance.
    #[test]
    fn resolve_ac_falls_back_to_tasks_acceptance_when_no_document() {
        let db = test_db();
        let card = make_card(None, Some("criterion one\ncriterion two"));
        let (criteria, source) = resolve_acceptance_criteria(&db, &card).expect("resolve");
        assert_eq!(criteria, vec!["criterion one", "criterion two"]);
        assert_eq!(source, "card");
    }

    /// When document_id present but document has no verification block,
    /// falls back to tasks.acceptance.
    #[test]
    fn resolve_ac_falls_back_when_doc_has_no_verification_block() {
        let db = test_db();
        let meta = DocumentMeta {
            id: Some("doc-no-ver"),
            doc_type: "requirement",
            surface: Some("test"),
            status: Some("draft"),
            priority: None,
            owner: "legion",
        };
        let payload = serde_json::json!({
            "meta": {"id": "doc-no-ver", "type": "requirement", "surface": "test",
                     "status": "draft", "priority": "SHOULD", "owner": "legion",
                     "date": "2026-06-12", "author": "test"},
            "title": "Test",
            "description": "desc",
            "traces_to": "x",
            "depends_on": []
        })
        .to_string();
        db.insert_document(&meta, &payload).expect("insert");

        let card = make_card(Some("doc-no-ver"), Some("fallback criterion"));
        let (criteria, source) = resolve_acceptance_criteria(&db, &card).expect("resolve");
        assert_eq!(criteria, vec!["fallback criterion"]);
        assert_eq!(source, "card");
    }

    /// When document has a non-empty verification.acceptance array,
    /// those strings are the criteria.
    #[test]
    fn resolve_ac_uses_spec_verification_acceptance_when_present() {
        let db = test_db();
        let meta = DocumentMeta {
            id: Some("doc-with-ver"),
            doc_type: "requirement",
            surface: Some("test"),
            status: Some("draft"),
            priority: None,
            owner: "legion",
        };
        let payload = serde_json::json!({
            "meta": {"id": "doc-with-ver", "type": "requirement", "surface": "test",
                     "status": "draft", "priority": "SHOULD", "owner": "legion",
                     "date": "2026-06-12", "author": "test"},
            "title": "Test",
            "description": "desc",
            "traces_to": "x",
            "depends_on": [],
            "verification": {
                "acceptance": [
                    "spec criterion alpha",
                    "spec criterion beta"
                ]
            }
        })
        .to_string();
        db.insert_document(&meta, &payload).expect("insert");

        // tasks.acceptance says something different -- the spec wins.
        let card = make_card(Some("doc-with-ver"), Some("should be ignored"));
        let (criteria, source) = resolve_acceptance_criteria(&db, &card).expect("resolve");
        assert_eq!(
            criteria,
            vec!["spec criterion alpha", "spec criterion beta"]
        );
        assert_eq!(source, "spec:doc-with-ver");
    }

    /// When the verification.acceptance array is empty, falls back to tasks.acceptance.
    #[test]
    fn resolve_ac_falls_back_when_verification_acceptance_is_empty() {
        let db = test_db();
        let meta = DocumentMeta {
            id: Some("doc-empty-ver"),
            doc_type: "requirement",
            surface: Some("test"),
            status: Some("draft"),
            priority: None,
            owner: "legion",
        };
        let payload = serde_json::json!({
            "meta": {"id": "doc-empty-ver", "type": "requirement", "surface": "test",
                     "status": "draft", "priority": "SHOULD", "owner": "legion",
                     "date": "2026-06-12", "author": "test"},
            "title": "Test",
            "description": "desc",
            "traces_to": "x",
            "depends_on": [],
            "verification": {"acceptance": []}
        })
        .to_string();
        db.insert_document(&meta, &payload).expect("insert");

        let card = make_card(Some("doc-empty-ver"), Some("fallback when spec empty"));
        let (criteria, source) = resolve_acceptance_criteria(&db, &card).expect("resolve");
        assert_eq!(criteria, vec!["fallback when spec empty"]);
        assert_eq!(source, "card");
    }
}
