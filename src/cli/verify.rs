//! `legion verify` and `legion quality-gate` handlers (carved from main.rs, #610).

use std::str::FromStr;

use clap::Subcommand;

use crate::cli::util::{
    git_changed_files, git_head_commit_and_branch, open_db, read_file_or_stdin,
};
use crate::db::quality_gates::{
    QualityGateFilter, QualityGateInput, QualityGateRow, QualityGateStats,
};
use crate::gate_trust::emit_gate_trust;
use crate::verify::{GateProvenance, GateResult};
use crate::{error, gate_registry, kanban, simplify_check, verify};

#[derive(Subcommand, Debug)]
pub(crate) enum QualityGateAction {
    /// Record a quality gate result for the current HEAD commit.
    ///
    /// Reads git HEAD and branch automatically. The skill runner calls this
    /// after inspecting the diff. `legion pr create` checks the gate before
    /// calling the work source so the result cannot be faked via a file flag.
    ///
    /// The row is recorded with ASSERTED provenance (#780): no validator
    /// backs it. For a skill with a check validator
    /// (`gate_registry::has_check_validator` -- `legion-simplify`,
    /// `legion-pr-write`), `--result clean` is REFUSED here -- a clean
    /// verdict for those skills can only be earned via `quality-gate check`,
    /// which validates a substantive articulation before recording. Skills
    /// with no check validator (`legion-review`, a `legion-verify:<card_id>`
    /// verdict) are unaffected: `record` is their only, legitimate path.
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

    /// List recorded quality gate rows, newest first.
    ///
    /// Filterable by skill, result, branch, and a since timestamp.
    /// Default output is a human-readable table; --json emits an array
    /// of objects that includes the details field.
    List {
        /// Restrict to rows for this skill name.
        #[arg(long)]
        skill: Option<String>,

        /// Restrict to rows with this result value: "clean" or "issues".
        #[arg(long, value_parser = ["clean", "issues"])]
        result: Option<String>,

        /// Restrict to rows on this branch.
        #[arg(long)]
        branch: Option<String>,

        /// Restrict to rows recorded at or after this RFC3339 timestamp.
        #[arg(long)]
        since: Option<String>,

        /// Emit JSON array instead of a human table.
        #[arg(long)]
        json: bool,
    },

    /// Show per-skill aggregate statistics.
    ///
    /// Prints runs, clean count, issues count, catch rate (issues/runs),
    /// total findings, and max findings for each skill. The catch rate is
    /// the rubberstamp tripwire: a rate near zero means the gate is not
    /// catching anything. --json emits structured rows.
    Stats {
        /// Restrict to this skill name.
        #[arg(long)]
        skill: Option<String>,

        /// Restrict to rows recorded at or after this RFC3339 timestamp.
        #[arg(long)]
        since: Option<String>,

        /// Emit JSON array instead of a human table.
        #[arg(long)]
        json: bool,
    },

    /// Validate a simplify articulation file before recording the gate (#665).
    ///
    /// Resolves the changed-file set from
    /// `git -c core.quotePath=false diff --name-status -M50% <base>...HEAD`
    /// (three-dot merge-base range; `<base>` is `--base` when given, else
    /// `main`, falling back to `origin/main...HEAD` when `main` is absent).
    /// If no base ref resolves and HEAD has a parent commit, this hard-errors
    /// rather than recording a gate against an empty set; an explicit
    /// `--base` that does not resolve is likewise a hard error (#779). Pure
    /// (zero-delta, R100) renames are auto-cleared from the coverage set --
    /// their old/new path pairs are recorded in the gate's `details` JSON
    /// instead of requiring an articulation entry, since a byte-identical
    /// move carries no simplification risk by construction. Renames with a
    /// content delta (R<100) still require an entry under the new path.
    /// Parses the articulation file -- markdown with one `### <path>`
    /// heading per changed file followed by prose -- and refuses when:
    ///   - Coverage gap: a changed file has no `### <path>` entry (reports
    ///     which files are unaddressed).
    ///   - Boilerplate / thin: an entry's prose is below the substance threshold
    ///     (reuses the same word-count heuristic as `pr write-check`).
    ///
    /// On pass: records a quality gate for HEAD under `--skill` with
    /// `--result` as the gate outcome, and the resolved base ref on the
    /// gate row's `base` column. On failure: lists each gap and exits
    /// non-zero without recording a gate.
    ///
    /// Mirror of `legion pr write-check` for the simplify gate. The
    /// `--result` flag carries the skill's own verdict (clean = no findings;
    /// issues = real simplify findings were found). The validator gates on
    /// articulation completeness and substance independently of that verdict:
    /// a clean result still requires a complete articulation.
    Check {
        /// Skill name to record the gate under (e.g. "legion-simplify").
        #[arg(long)]
        skill: String,

        /// Gate result from the skill run: "clean" or "issues".
        #[arg(long, value_parser = ["clean", "issues"])]
        result: String,

        /// Path to the markdown articulation file. Reads stdin when omitted.
        #[arg(long)]
        articulation_file: Option<String>,

        /// Number of skill findings (default 0; used when --result is "issues").
        #[arg(long, default_value = "0")]
        findings_count: u64,

        /// Override the base ref the changed-file set is diffed against
        /// (default: `main`, falling back to `origin/main`). For stacked
        /// branches whose parent is an unmerged feature branch, pass that
        /// branch so the coverage set is scoped to what this branch actually
        /// changed rather than everything since `main` (#779). Must resolve
        /// to a real ref -- an unresolvable `--base` is a hard error, same as
        /// the no-base-ref case with no override. The resolved base is
        /// recorded on the gate row regardless of whether it came from this
        /// flag or the default resolution, so a too-narrow base stays
        /// visible in the audit trail.
        #[arg(long)]
        base: Option<String>,
    },

    /// Void a gate row: retire a known-false verdict without deleting it
    /// from history (#780 tombstone pattern, mirroring `deleted_at` on
    /// tasks/reflections/schedules).
    ///
    /// A voided row drops out of `get_quality_gate` /
    /// `get_latest_quality_gate_by_skill` (so `pr create`'s gate check and
    /// the ->Done gate can never resolve to it again) and out of
    /// `quality-gate stats`, but stays visible in `quality-gate list` --
    /// voiding annotates the ledger, it never erases it.
    ///
    /// Use `--superseded-by` once the genuine replacement row exists (e.g.
    /// after re-running `quality-gate check` on the same commit) to link the
    /// voided row to what replaced it.
    Void {
        /// Id of the gate row to void (from `quality-gate list` or the id
        /// printed by `record`/`check`).
        #[arg(long)]
        id: String,

        /// Why this row is known-false (required -- a void with no reason
        /// is not an audit trail).
        #[arg(long)]
        reason: String,

        /// Id of the row that supersedes this one, if a re-laid genuine row
        /// already exists.
        #[arg(long)]
        superseded_by: Option<String>,
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

            // #780: a "clean" verdict for a skill with a check validator can
            // only be earned by passing that validator. Refusing here closes
            // the exact loophole a manufactured-clean row exploits -- self-
            // reporting "clean" via `record` for a skill whose real gate is
            // `check`. Skills with no validator (legion-review, a
            // legion-verify:<card_id> verdict) are asserted by necessity and
            // unaffected.
            if gate_result == GateResult::Clean && gate_registry::has_check_validator(&skill) {
                eprintln!(
                    "[legion] error: '{skill}' has a check validator -- a clean gate cannot be \
                     recorded via 'quality-gate record'. Run 'quality-gate check --skill {skill} \
                     --result clean ...' instead, which validates a substantive per-changed-file \
                     articulation before recording."
                );
                return Err(error::LegionError::ExitWith(1));
            }

            let (commit_hash, branch) = git_head_commit_and_branch()?;

            let database = open_db()?;
            let row = database.record_quality_gate(&QualityGateInput {
                branch: &branch,
                commit_hash: &commit_hash,
                skill: &skill,
                result: gate_result,
                findings_count,
                details: details_json.as_deref(),
                provenance: GateProvenance::Asserted,
                base: None,
            })?;
            emit_gate_trust(&database, &row);
            // Phase 2b: a downstream legion-review verdict witnesses the
            // upstream legion-simplify gate prediction for this commit -- review
            // catching issues means simplify's clean verdict was wrong.
            crate::gate_trust::maybe_witness_from_review(&database, &row);
            println!("{}", row.id);
        }

        QualityGateAction::List {
            skill,
            result,
            branch,
            since,
            json,
        } => {
            // Parse the optional --result flag into a typed GateResult so an
            // invalid value surfaces a descriptive error before we touch the DB.
            let gate_result: Option<GateResult> = match result.as_deref() {
                Some(r) => Some(GateResult::from_str(r)?),
                None => None,
            };

            let database = open_db()?;
            let rows: Vec<QualityGateRow> = database.list_quality_gates(&QualityGateFilter {
                skill,
                result: gate_result,
                branch,
                since,
            })?;

            if json {
                println!("{}", serde_json::to_string(&rows)?);
            } else {
                print_gate_table(&rows);
            }
        }

        QualityGateAction::Stats { skill, since, json } => {
            let database = open_db()?;
            let stats: Vec<QualityGateStats> =
                database.quality_gate_stats(skill.as_deref(), since.as_deref())?;

            if json {
                println!("{}", serde_json::to_string(&stats)?);
            } else {
                print_stats_table(&stats);
            }
        }

        QualityGateAction::Check {
            skill,
            result,
            articulation_file,
            findings_count,
            base,
        } => {
            // Parse and validate the result flag before touching the FS.
            let gate_result: GateResult = GateResult::from_str(&result)?;

            // Resolve the changed-file set from git. `--base` overrides the
            // default main/origin-main resolution (#779); an unresolvable
            // `--base` hard-errors rather than falling back silently. Pure
            // (R100) renames are cleared from `files` and carried separately
            // in `cleared_renames` for the audit trail.
            let changed = git_changed_files(base.as_deref())?;

            // Read the articulation from --articulation-file or stdin.
            let articulation =
                read_file_or_stdin(articulation_file.as_deref(), "--articulation-file")?;

            let report = simplify_check::validate_articulation(&changed.files, &articulation);

            // The gate is only recorded when the articulation passes the
            // structural validator. A failed articulation exits non-zero
            // without recording so the gate on HEAD stays absent (pr create
            // will refuse until a valid articulation is submitted).
            if !report.ok {
                let gap_count = report.findings.len();
                eprintln!(
                    "[legion] simplify-check FAILED for skill '{skill}' -- {gap_count} gap(s):"
                );
                for f in &report.findings {
                    eprintln!("  - {f}");
                }
                eprintln!(
                    "\nThe articulation must have one `### <path>` entry per changed file, \
                     each with composed prose explaining which simplify checks were applied \
                     and the reasoning for the clean-or-finding verdict. Fix the articulation \
                     and re-run."
                );
                return Err(error::LegionError::ExitWith(1));
            }

            // Articulation is valid. Record the gate under HEAD.
            // findings_count is the skill's own count (real simplify findings),
            // not the validator's gap count (which is 0 when we reach here).
            // It is valid for --result issues to carry --findings-count 0: the
            // flag is informational, and the skill runner may not always surface
            // a count. The gate result is what matters for `legion pr create`.
            let (commit_hash, branch) = git_head_commit_and_branch()?;
            // Cleared (R100) renames are excluded from `report`'s coverage
            // requirement but still surfaced here -- count + pairs -- so the
            // exclusion is auditable rather than silent (#779).
            let cleared_renames_json: Vec<serde_json::Value> = changed
                .cleared_renames
                .iter()
                .map(|(old, new)| serde_json::json!({"old": old, "new": new}))
                .collect();
            let details = serde_json::json!({
                "skill": skill,
                "result": result,
                "entry_count": report.entry_count,
                "findings_count": findings_count,
                "articulation": articulation,
                "base": changed.base,
                "cleared_renames_count": cleared_renames_json.len(),
                "cleared_renames": cleared_renames_json,
            })
            .to_string();

            let database = open_db()?;
            let row = database.record_quality_gate(&QualityGateInput {
                branch: &branch,
                commit_hash: &commit_hash,
                skill: &skill,
                result: gate_result,
                findings_count,
                details: Some(&details),
                provenance: GateProvenance::Validated,
                base: changed.base.as_deref(),
            })?;
            emit_gate_trust(&database, &row);

            println!(
                "[legion] simplify-check articulation accepted for skill '{skill}' \
                 (result '{result}', {} file entries, {findings_count} skill findings, \
                 base '{}', {} rename(s) auto-cleared). Gate id: {}",
                report.entry_count,
                changed.base.as_deref().unwrap_or("<none>"),
                changed.cleared_renames.len(),
                row.id,
            );
        }

        QualityGateAction::Void {
            id,
            reason,
            superseded_by,
        } => {
            let database = open_db()?;
            let row = database.void_quality_gate(&id, &reason, superseded_by.as_deref())?;
            println!(
                "[legion] voided gate {} (skill '{}', commit {}): {}",
                row.id, row.skill, row.commit_hash, reason
            );
            if let Some(sup) = &row.superseded_by {
                println!("  superseded by: {sup}");
            }
        }
    }
    Ok(())
}

/// Print gate rows as a human-readable table to stdout.
///
/// Columns: id (first 8 chars), branch, commit (first 8 chars), skill,
/// result, findings, provenance, void, created_at. An empty slice prints
/// nothing.
///
/// PROVENANCE and VOID surface #780's audit distinction on the table a human
/// actually reads by default: PROVENANCE separates a structurally VALIDATED
/// clean from a merely ASSERTED one, and VOID marks a row retired as
/// known-false ("-" for a live row, "VOID" for a voided one) so a retired
/// row never visually blends in with a live one. `--json` (see
/// `QualityGateRow`'s `Serialize`) already carries the full
/// `voided_at`/`void_reason`/`superseded_by` detail for tooling; this table
/// is the quick-glance surface.
fn print_gate_table(rows: &[QualityGateRow]) {
    if rows.is_empty() {
        return;
    }
    println!(
        "{:<8}  {:<20}  {:<8}  {:<22}  {:<6}  {:>8}  {:<9}  {:<4}  CREATED",
        "ID", "BRANCH", "COMMIT", "SKILL", "RESULT", "FINDINGS", "PROVENANCE", "VOID"
    );
    println!("{}", "-".repeat(130));
    for row in rows {
        let id_short: String = row.id.chars().take(8).collect();
        let branch_trunc: String = row.branch.chars().take(20).collect();
        let commit_short: String = row.commit_hash.chars().take(8).collect();
        let skill_trunc: String = row.skill.chars().take(22).collect();
        let void_marker = if row.voided_at.is_some() { "VOID" } else { "-" };
        println!(
            "{:<8}  {:<20}  {:<8}  {:<22}  {:<6}  {:>8}  {:<9}  {:<4}  {}",
            id_short,
            branch_trunc,
            commit_short,
            skill_trunc,
            row.result.as_str(),
            row.findings_count,
            row.provenance.as_str(),
            void_marker,
            row.created_at,
        );
    }
}

/// Print per-skill stats as a human-readable table to stdout.
///
/// Columns: skill, runs, clean, issues, catch_rate (%), total_findings,
/// max_findings. An empty slice prints nothing.
fn print_stats_table(stats: &[QualityGateStats]) {
    if stats.is_empty() {
        return;
    }
    println!(
        "{:<25}  {:>5}  {:>5}  {:>6}  {:>10}  {:>14}  {:>12}",
        "SKILL", "RUNS", "CLEAN", "ISSUES", "CATCH_RATE", "TOTAL_FINDINGS", "MAX_FINDINGS"
    );
    println!("{}", "-".repeat(88));
    for s in stats {
        println!(
            "{:<25}  {:>5}  {:>5}  {:>6}  {:>9.1}%  {:>14}  {:>12}",
            s.skill,
            s.runs,
            s.clean,
            s.issues,
            s.catch_rate * 100.0,
            s.total_findings,
            s.max_findings,
        );
    }
}

/// Resolve acceptance criteria for a card, with spec-document precedence (#528, #644).
///
/// Shared by `handle_verify` and `handle_done` so both gates key on the same
/// AC source: spec-bound cards gate on the bound document's
/// `verification.acceptance`, not `tasks.acceptance`.
///
/// Returns `(criteria, source_label)`.
///
/// Precedence:
/// 1. When the card has a `document_id` AND the bound document's payload has a
///    non-empty `verification.acceptance` array, those strings become the AC.
///    Source label: `"spec:<document_id>"`.
/// 2. When the card has a `document_id` but the document cannot be found, this
///    is a hard error -- a bound card whose spec has vanished must not silently
///    fall back; verify must not paper over a dangling reference. This matches
///    the behavior of `transition_card_status_with_sync`, which also hard-errors
///    on a missing bound document.
/// 3. When the bound document exists but has no `verification` block, or the
///    `verification.acceptance` array is empty, or the payload cannot be parsed
///    (corrupt doc), falls back to `tasks.acceptance` with source `"card"`.
/// 4. When the card has no `document_id`: `tasks.acceptance`. Source `"card"`.
pub(crate) fn resolve_acceptance_criteria(
    database: &crate::db::Database,
    card: &kanban::Card,
) -> error::Result<(Vec<String>, String)> {
    if let Some(ref doc_id) = card.document_id {
        // The document must exist. A dangling document_id is a hard error: the
        // spec that was authoritative has been deleted while work was in flight.
        let doc = database.get_document(doc_id)?.ok_or_else(|| {
            error::LegionError::WorkSource(format!(
                "card '{}' has document_id '{doc_id}' but the document does not exist; \
                 verify cannot proceed with a dangling spec reference",
                card.id
            ))
        })?;

        // Parse the payload and look for verification.acceptance. A corrupt
        // payload or a missing verification block is non-fatal: fall back to
        // tasks.acceptance so a structural gap in the spec does not hard-block
        // verify (the intent is that the human fills in the spec).
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(&doc.payload)
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
        // Document exists but has no usable verification.acceptance: fall back.
    }
    // No document_id, or document exists without usable verification.acceptance.
    let criteria = verify::acceptance_items(card.acceptance.as_deref());
    Ok((criteria, "card".to_string()))
}

pub(crate) fn handle_verify(
    card: String,
    verdicts_file: Option<String>,
    deviation: Option<String>,
) -> error::Result<()> {
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

    // Spec-revision deviation gate (#554,
    // docs/decisions/2026-05-31-spec-revision-protocol.md). Checked before
    // reading verdicts: an unratified deviation hard-blocks regardless of
    // what the verdicts file claims, because the presence of a ratified
    // `ReplanRecord` -- not the agent's self-reported verdicts -- is the
    // signal that distinguishes a sanctioned re-plan from improvisation.
    let ratified_replan_exists = database
        .get_latest_replan_record(&card)?
        .is_some_and(|r| r.ratified);
    if let Some(verify::VerifyDecision::ReplanRequired { reason }) =
        verify::replan_gate(deviation.as_deref(), ratified_replan_exists)
    {
        let (commit_hash, branch) = git_head_commit_and_branch()?;
        let skill = verify::verify_gate_key(&card);
        let details = serde_json::json!({
            "skill": "legion-verify",
            "card": card,
            "decision": format!("ReplanRequired: {reason}"),
        })
        .to_string();
        database.record_quality_gate(&QualityGateInput {
            branch: &branch,
            commit_hash: &commit_hash,
            skill: &skill,
            result: GateResult::Issues,
            findings_count: 1,
            details: Some(&details),
            // legion-verify has no check validator -- asserted by necessity
            // (#780), same as every other verify-recorded row below.
            provenance: GateProvenance::Asserted,
            base: None,
        })?;
        eprintln!("[legion] verify BLOCKED for card {card}: {reason}");
        return Err(error::LegionError::ExitWith(1));
    } else if let Some(reason) = deviation.as_deref() {
        // A deviation was asserted but a ratified ReplanRecord covers it --
        // proceed against the current (revised) AC via the normal decide()
        // path below. Leave a breadcrumb so the asserted reason is not
        // silently dropped from the audit trail.
        eprintln!(
            "[legion] verify: deviation ratified for card {card} ({reason}) -- \
             auditing against the revised acceptance criteria"
        );
    }

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
        verify::VerifyDecision::ReplanRequired { .. } => 1,
        verify::VerifyDecision::Proceed => 0,
    };
    let gate_result = if decision.allows_done() {
        GateResult::Clean
    } else {
        GateResult::Issues
    };
    database.record_quality_gate(&QualityGateInput {
        branch: &branch,
        commit_hash: &commit_hash,
        skill: &skill,
        result: gate_result,
        findings_count: findings,
        details: Some(&details),
        // legion-verify has no check validator -- asserted by necessity (#780).
        provenance: GateProvenance::Asserted,
        base: None,
    })?;

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
        // `decide()` never produces this variant itself -- it is returned
        // only by `verify::replan_gate`, which `handle_verify` checks (and
        // returns early on) before `decide()` runs. Covered here for
        // exhaustiveness against future callers of `decide()`.
        verify::VerifyDecision::ReplanRequired { reason } => {
            eprintln!("[legion] verify BLOCKED for card {card}: {reason}");
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

    /// When the card has a document_id that refers to a non-existent document,
    /// resolve_acceptance_criteria must hard-error. A dangling reference means
    /// the spec was deleted while work was in flight; verify must not silently
    /// fall back to tasks.acceptance in that state.
    #[test]
    fn resolve_ac_hard_errors_on_dangling_document_id() {
        let db = test_db();
        // Card points at a document that was never inserted.
        let card = make_card(Some("nonexistent-doc-id"), Some("card fallback criterion"));
        let err = resolve_acceptance_criteria(&db, &card)
            .expect_err("expected hard error on dangling document_id");
        assert!(
            err.to_string().contains("nonexistent-doc-id"),
            "error must name the missing document id, got: {err}"
        );
    }

    // --- simplify-check gate tests (#665) ---

    /// A valid articulation covering all changed files records a clean gate
    /// in the quality_gates table under the given skill + commit.
    #[test]
    fn simplify_check_gate_recorded_on_valid_articulation() {
        use std::collections::HashSet;

        use crate::simplify_check::validate_articulation;
        use crate::verify::GateResult;

        let db = test_db();
        let changed: HashSet<String> = ["src/foo.rs", "src/bar.rs"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let articulation = "### src/foo.rs\n\
             Checked all six categories. No duplicate logic found: `fn handle_foo` \
             at src/foo.rs:30 handles exactly one concern. No stringly-typed state; \
             enums used throughout. Error handling propagates via the ? operator.\n\
             ### src/bar.rs\n\
             Reviewed for unnecessary abstraction and copy-paste variation. The \
             single trait bound on `fn render` at src/bar.rs:88 is load-bearing -- \
             removing it would require duplicating the impl block in three callers. \
             Clean verdict: no simplify findings.\n";

        let report = validate_articulation(&changed, articulation);
        assert!(
            report.ok,
            "expected valid articulation, got {:?}",
            report.findings
        );

        // Simulate what the handler does: record the gate.
        let row = db
            .record_quality_gate(&QualityGateInput {
                branch: "feat/665-simplify-articulation",
                commit_hash: "deadbeefdeadbeef",
                skill: "legion-simplify",
                result: GateResult::Clean,
                findings_count: 0,
                details: Some(&serde_json::json!({"articulation": articulation}).to_string()),
                provenance: GateProvenance::Validated,
                base: None,
            })
            .expect("record_quality_gate failed");
        assert!(!row.id.is_empty());

        // Verify it can be retrieved by the commit + skill pair.
        let fetched = db
            .get_quality_gate("deadbeefdeadbeef", "legion-simplify")
            .expect("get_quality_gate failed")
            .expect("expected Some gate row");
        assert_eq!(fetched.result, GateResult::Clean);
        assert_eq!(fetched.skill, "legion-simplify");
    }

    /// A missing-coverage gap causes the validator to refuse. The gate should
    /// NOT be recorded (the handler exits non-zero before touching the DB).
    #[test]
    fn simplify_check_refuses_missing_coverage() {
        use std::collections::HashSet;

        use crate::simplify_check::validate_articulation;

        let changed: HashSet<String> = ["src/foo.rs", "src/missing.rs"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let articulation = "### src/foo.rs\n\
             Checked all six simplify categories. No findings: each function \
             is focused on a single concern, types are explicit, error handling \
             propagates via ? throughout the module.\n";

        let report = validate_articulation(&changed, articulation);
        assert!(!report.ok, "expected refusal for missing coverage");
        assert!(
            report
                .findings
                .iter()
                .any(|f| f.contains("src/missing.rs") && f.contains("missing coverage")),
            "expected a missing-coverage finding naming src/missing.rs, got {:?}",
            report.findings
        );
    }

    /// A boilerplate entry (restates category names without reasoning, under
    /// the word threshold) causes the validator to refuse.
    #[test]
    fn simplify_check_refuses_boilerplate_entry() {
        use std::collections::HashSet;

        use crate::simplify_check::validate_articulation;

        let changed: HashSet<String> = ["src/foo.rs"].iter().map(|s| s.to_string()).collect();
        // Entry only lists the check names -- not enough words or reasoning.
        let articulation = "### src/foo.rs\nClean. No issues.\n";

        let report = validate_articulation(&changed, articulation);
        assert!(!report.ok, "expected refusal for thin entry");
        assert!(
            report
                .findings
                .iter()
                .any(|f| f.contains("too thin") && f.contains("src/foo.rs")),
            "expected a thin-entry finding, got {:?}",
            report.findings
        );
    }

    /// An articulation with real findings (issues result) still passes the
    /// structural validator if coverage and substance are present.
    #[test]
    fn simplify_check_accepts_issues_result_with_substantive_articulation() {
        use std::collections::HashSet;

        use crate::simplify_check::validate_articulation;

        let changed: HashSet<String> = ["src/foo.rs"].iter().map(|s| s.to_string()).collect();
        let articulation = "### src/foo.rs\n\
             Checked for duplicate logic: found two match arms at lines 47 and \
             62 that share an identical body. Extracted into a helper \
             `fn apply_default` to remove the copy-paste variation. No other \
             issues found: stringly-typed state is absent, error handling uses \
             ? throughout, no hand-rolled standard library duplication.\n";

        let report = validate_articulation(&changed, articulation);
        assert!(
            report.ok,
            "issues result with substantive articulation should pass the structural validator, \
             got {:?}",
            report.findings
        );
    }
}
