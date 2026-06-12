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

pub(crate) fn handle_verify(card: String, verdicts_file: Option<String>) -> error::Result<()> {
    let database = open_db()?;

    let card_row = database
        .get_card_by_id(&card)?
        .ok_or_else(|| error::LegionError::CardNotFound(card.clone()))?;
    let acceptance = verify::acceptance_items(card_row.acceptance.as_deref());

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
                "[legion] verify PASS for card {card} ({} criteria). ->Done is unblocked.",
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
