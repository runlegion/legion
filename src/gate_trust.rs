//! Gate-trust: emit a quality-gate verdict as an uncertainty-engine prediction.
//!
//! A gate "clean" verdict is a forecast ("this diff has no issues") at an
//! implied confidence near 1.0. To measure the rubber-stamp rate per cohort we
//! record each verdict as a `surface=legion.gate` prediction; a later witness
//! (Phase 2b) trues it against downstream ground truth (an independent pre-push
//! review catch, a revert, a post-merge bug). This module is a CALLER of the
//! uncertainty engine -- it builds a `PredictionInput` and inserts it. It does
//! not touch the engine's logic.
//!
//! The emit is deliberately NON-BLOCKING at the call sites: a failure here must
//! never break gate recording, mirroring the `uncertainty-emit-on-task` hook.
//!
//! Re-run contract: re-running the same gate on the same commit (a normal
//! iteration -- fix, re-check) emits ANOTHER prediction with the same
//! `skill:commit` fingerprint but a fresh id. This is intentional and harmless:
//! the Phase 2b witness resolves a fingerprint by taking the LATEST emitted
//! prediction (the gate run that actually counts), and any earlier unwitnessed
//! duplicates simply orphan out after the TTL and are excluded from
//! calibration. The fingerprint identifies the (skill, commit) cohort run, not a
//! unique row.
//!
//! DECORRELATED WITNESS (#694): `witness_gate_external` witnesses a
//! `legion.gate` prediction from a source outside the pipeline -- today an
//! operator via `legion uncertainty witness-gate`, the CLI option named in
//! #694 -- so both directions (correct == true and correct == false) are
//! ground truth rather than a downstream skill's own optimism. The other two
//! #694 options (an independent reviewer on a different model; git-revert /
//! post-merge-bug detection) remain future work -- see the PR body for why
//! the operator-CLI source was chosen as the smallest compliant mechanism.

use crate::db::Database;
use crate::db::quality_gates::QualityGateRow;
use crate::gate_registry;
use crate::uncertainty::error::Result as UncertaintyResult;
use crate::uncertainty::storage::orphan_after_from_ttl;
use crate::uncertainty::types::{
    Confidence, Correctness, OutcomeLabel, Prediction, PredictionInput, PredictionState,
};
use crate::verify::{GateProvenance, GateResult};

/// The uncertainty surface all gate verdicts emit under.
pub const GATE_SURFACE: &str = "legion.gate";

/// Days a gate prediction waits for a witness before the engine's sweep
/// orphans it (orphans are excluded from calibration).
const GATE_ORPHAN_TTL_DAYS: u32 = 30;

/// Claimed P(clean) for a `clean` verdict. Degenerate by design -- every
/// "clean" verdict pins near 1.0; the signal is the witnessed correctness, not
/// this number. Kept just below 1.0 so it lands in the engine's top bucket
/// without being a literal certainty.
const CLEAN_CONFIDENCE: f64 = 0.95;

/// Claimed P(clean) for an `issues` verdict -- the gate is asserting the diff
/// is NOT clean.
const ISSUES_CONFIDENCE: f64 = 0.05;

/// Deterministic fingerprint for a (skill, commit) gate run. Both components
/// are already safe, stable identifiers (the commit is a git hash, the skill a
/// short slug), so a plain join is a sufficient lookup key -- the Phase 2b
/// witness recomputes it to find this prediction. No hashing is needed; hashing
/// would only obscure an already-unique key.
///
/// EXPECTED: `skill` contains no `:`. Gate skill names are colon-free slugs
/// (`legion-review`, `legion-pr-write`) and commit hashes are
/// hex, so the join is unambiguous. The fingerprint is treated as an OPAQUE key
/// (recomputed and compared, never parsed back into parts), so a stray colon
/// only risks a (vanishingly unlikely) collision between two distinct (skill,
/// commit) pairs, never a parse error -- so this deliberately does not assert
/// the expectation: `witness_gate_external` (#694) passes an operator-supplied
/// `--skill` value through to this function, and a typo'd colon in that free
/// text must degrade to a harmless lookup miss, not a panic.
pub fn gate_fingerprint(skill: &str, commit_hash: &str) -> String {
    format!("{skill}:{commit_hash}")
}

/// The model id of the agent that ran the gate, for cohorting by who
/// rubber-stamps. Sourced from the environment; "unknown" when the harness does
/// not surface it (cohorts still distinguish gate type and legion version).
fn agent_model() -> String {
    std::env::var("LEGION_AGENT_MODEL").unwrap_or_else(|_| "unknown".to_string())
}

fn confidence_for(result: GateResult) -> f64 {
    match result {
        GateResult::Clean => CLEAN_CONFIDENCE,
        GateResult::Issues => ISSUES_CONFIDENCE,
    }
}

/// Build the prediction input for a recorded gate verdict.
///
/// Mapping: surface=legion.gate, feature_key=gate.<skill>, model=agent model,
/// model_version=legion's own version (so a release that changes rubber-stamp
/// behavior shows up as a new cohort), fingerprint=skill:commit, confidence by
/// verdict, payload carrying the gate context, and `issue_ref` (#1279) when
/// the recorder knows which issue the gated branch is for.
fn prediction_input(
    row: &QualityGateRow,
    issue_ref: Option<&str>,
) -> UncertaintyResult<PredictionInput> {
    let payload = serde_json::json!({
        "skill": row.skill,
        "branch": row.branch,
        "commit": row.commit_hash,
        "findings_count": row.findings_count,
        "result": row.result.as_str(),
    });
    Ok(PredictionInput {
        surface: GATE_SURFACE.to_string(),
        feature_key: format!("gate.{}", row.skill),
        input_fingerprint: gate_fingerprint(&row.skill, &row.commit_hash),
        model: agent_model(),
        model_version: env!("CARGO_PKG_VERSION").to_string(),
        claimed_confidence: Confidence::from_f64(confidence_for(row.result))?,
        prediction_payload: payload,
        orphan_after: orphan_after_from_ttl(GATE_ORPHAN_TTL_DAYS),
        // Keyed on (skill, commit) either way; the issue rides along only
        // when the recorder knows it, so verify can find the verdict (#1279).
        issue_ref: issue_ref.map(str::to_owned),
    })
}

/// Emit a gate verdict as an uncertainty prediction. Returns the prediction id
/// on success. The non-blocking wrapper `emit_gate_trust` is what call sites
/// use; this inner function returns the Result for tests.
pub fn emit_gate_prediction(
    db: &Database,
    row: &QualityGateRow,
    issue_ref: Option<&str>,
) -> UncertaintyResult<String> {
    let prediction = Prediction::new(prediction_input(row, issue_ref)?);
    db.insert_prediction(&prediction)?;
    Ok(prediction.id)
}

/// True when `row` must NOT be ingested as gate-trust ground truth (#780): an
/// ASSERTED "clean" verdict for a skill that has a check validator
/// (`gate_registry::has_check_validator`). Recording such a row is already
/// refused at the CLI layer (`cli::verify::handle_quality_gate`'s `Record`
/// arm), but this is the defense-in-depth boundary for the ingestion path
/// itself -- a pre-#780 historical row, or any future caller that reaches
/// `record_quality_gate` without going through that CLI refusal, must still
/// never let a manufactured clean count as a passed gate. Admitting it would
/// let exactly the rubber-stamp this module measures poison its own
/// denominator.
///
/// A VALIDATED clean (the validator path) and an ASSERTED `issues` (a real
/// finding, self-reported or not, is not a ground-truth risk in the same
/// direction) are both eligible for ingestion as before.
fn is_ungrounded_assertion(row: &QualityGateRow) -> bool {
    row.result == GateResult::Clean
        && row.provenance == GateProvenance::Asserted
        && gate_registry::has_check_validator(&row.skill)
}

/// Non-blocking gate-trust emit for the gate-record handlers: log on failure,
/// never propagate. A gate-trust problem must never break gate recording or the
/// agent's workflow -- the measurement is best-effort, the gate is not.
///
/// Skips ingestion entirely (no prediction emitted) for an ungrounded
/// assertion (#780, see `is_ungrounded_assertion`) -- this is the boundary
/// acceptance criterion #3 names: gate-trust must not consume an
/// asserted-clean row for a check-gated skill as positive ground truth.
///
/// `issue_ref` (`<owner>/<repo>#<n>`, #1279) is the issue the gate was
/// recorded for, when the recorder knows it; `None` emits as before. A gate
/// row itself names only a branch, and branch names are not a mapping to
/// issues, so only a recorder that was handed the issue passes one.
pub fn emit_gate_trust(db: &Database, row: &QualityGateRow, issue_ref: Option<&str>) {
    if is_ungrounded_assertion(row) {
        eprintln!(
            "[legion] gate-trust: skipping ingestion of an ASSERTED clean verdict for \
             check-gated skill '{}' on commit {} -- a manufactured clean cannot count as \
             ground truth (#780). Re-run via its validator to validate and ingest it.",
            row.skill, row.commit_hash
        );
        return;
    }
    if let Err(e) = emit_gate_prediction(db, row, issue_ref) {
        eprintln!("[legion] gate-trust emit failed (non-fatal): {e}");
    }
}

/// Witness a `surface=legion.gate` prediction from a DECORRELATED external
/// source -- an operator or other ground truth outside the pipeline that
/// emitted the verdict (#694). An operator saying "this clean verdict was
/// actually wrong" (or actually right) is independent of the pipeline's own
/// rubber-stamp tendency in BOTH directions.
///
/// It does NOT skip a non-clean (`issues`) prediction: an operator witness is
/// direct ground truth about whichever verdict the gate actually recorded,
/// clean or issues, so both are eligible.
///
/// Resolves the prediction by the deterministic `(skill, commit)`
/// fingerprint, taking the latest Emitted row per the re-run contract (see
/// module docs). Returns `Ok(Some(prediction_id))` naming the witnessed row
/// when a matching Emitted prediction exists, `Ok(None)` if none does
/// (already witnessed/orphaned, or the gate was never recorded for that
/// commit) -- a clean no-op the CLI layer turns into a clear error for the
/// operator. The id lets the operator audit exactly which row their witness
/// touched, echoing the generic `legion uncertainty witness` command's habit
/// of returning the touched prediction's id.
pub fn witness_gate_external(
    db: &Database,
    skill: &str,
    commit_hash: &str,
    correct: bool,
) -> UncertaintyResult<Option<String>> {
    let fingerprint = gate_fingerprint(skill, commit_hash);
    let Some(prediction) = db.latest_emitted_by_fingerprint(GATE_SURFACE, &fingerprint)? else {
        return Ok(None);
    };
    witness_gate_prediction(db, prediction, correct).map(Some)
}

/// `witness_gate_external` for one prediction named by its id (#1415), so
/// two predictions recorded on one `(skill, commit)` -- a gate checked twice
/// on a commit -- are each witnessable. `Ok(None)` when the id names no
/// Emitted `legion.gate` prediction.
pub(crate) fn witness_gate_by_id(
    db: &Database,
    prediction_id: &str,
    correct: bool,
) -> UncertaintyResult<Option<String>> {
    match db.get_prediction(prediction_id)? {
        Some(p) if p.surface == GATE_SURFACE && p.state == PredictionState::Emitted => {
            witness_gate_prediction(db, p, correct).map(Some)
        }
        _ => Ok(None),
    }
}

/// The full commit hashes of every recorded `legion.gate` prediction for
/// `skill` whose commit starts with `commit_prefix` (#1415), so a short sha
/// resolves to the one commit it names. An exact match wins outright.
pub(crate) fn gate_commits_matching(
    db: &Database,
    skill: &str,
    commit_prefix: &str,
) -> UncertaintyResult<Vec<String>> {
    let skill_prefix = gate_fingerprint(skill, "");
    let commits: Vec<String> = db
        .fingerprints_with_prefix(GATE_SURFACE, &gate_fingerprint(skill, commit_prefix))?
        .into_iter()
        .filter_map(|fp| fp.strip_prefix(&skill_prefix).map(str::to_string))
        .collect();
    if commits.iter().any(|c| c == commit_prefix) {
        return Ok(vec![commit_prefix.to_string()]);
    }
    Ok(commits)
}

/// The witness tail shared by the `(skill, commit)` and by-id paths.
fn witness_gate_prediction(
    db: &Database,
    mut prediction: Prediction,
    correct: bool,
) -> UncertaintyResult<String> {
    // The claimed event on this surface is P(clean) (CLEAN_CONFIDENCE for a
    // `clean` verdict, ISSUES_CONFIDENCE for an `issues` verdict, which claims
    // NOT clean). `correct` is relative to whichever verdict the gate actually
    // recorded, so it must be reprojected onto the claimed event -- "actually
    // clean" -- before mapping to an outcome. For a `clean`
    // verdict, correct IS actually_clean. For an `issues` verdict, a correct
    // catch means it was NOT actually clean, so actually_clean is the
    // negation of `correct`. Skipping this reprojection (mapping `correct`
    // straight to Shipped/Escalated) inverts the signal for every `issues`
    // verdict.
    let was_clean = prediction
        .prediction_payload
        .get("result")
        .and_then(|v| v.as_str())
        == Some("clean");
    let actually_clean = if was_clean { correct } else { !correct };
    let (label, correctness) = if actually_clean {
        (OutcomeLabel::Shipped, 1.0)
    } else {
        (OutcomeLabel::Escalated, 0.0)
    };
    let now = chrono::Utc::now().to_rfc3339();
    let payload = serde_json::json!({
        "witnessed_by": "operator",
        "correct": correct,
    });
    // Captured before the in-memory transition below so the CAS write can
    // guard against the orphan sweep racing this read (#1003).
    let prev_state = prediction.state;
    prediction.witness(label, payload, Correctness::from_f64(correctness)?, &now)?;
    db.update_prediction(&prediction, prev_state)?;
    Ok(prediction.id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::testutil::test_db;

    /// Builds a row with VALIDATED provenance -- the common case every
    /// pre-#780 test in this module exercises (emission/witness mechanics
    /// unrelated to provenance), so defaulting here keeps their behavior
    /// unchanged. The #780 ingestion-guard tests build their own row with
    /// ASSERTED provenance via `gate_row_with_provenance` instead.
    fn gate_row(skill: &str, result: GateResult, findings: u64) -> QualityGateRow {
        gate_row_with_provenance(skill, result, findings, GateProvenance::Validated)
    }

    fn gate_row_with_provenance(
        skill: &str,
        result: GateResult,
        findings: u64,
        provenance: GateProvenance,
    ) -> QualityGateRow {
        QualityGateRow {
            id: "gate-id".to_string(),
            branch: "feat/x".to_string(),
            commit_hash: "deadbeefcafe".to_string(),
            skill: skill.to_string(),
            result,
            findings_count: findings,
            details: None,
            created_at: "2026-06-27T00:00:00+00:00".to_string(),
            provenance,
            voided_at: None,
            void_reason: None,
            superseded_by: None,
            base: None,
        }
    }

    #[test]
    fn fingerprint_is_deterministic_and_componentwise() {
        assert_eq!(
            gate_fingerprint("legion-simplify", "abc123"),
            "legion-simplify:abc123"
        );
        // Same inputs -> same fingerprint (the witness side relies on this).
        assert_eq!(
            gate_fingerprint("legion-simplify", "abc123"),
            gate_fingerprint("legion-simplify", "abc123")
        );
        // Different skill or commit -> different fingerprint.
        assert_ne!(
            gate_fingerprint("legion-simplify", "abc123"),
            gate_fingerprint("legion-review", "abc123")
        );
    }

    #[test]
    fn clean_and_issues_map_to_distinct_confidences() {
        assert_eq!(confidence_for(GateResult::Clean), CLEAN_CONFIDENCE);
        assert_eq!(confidence_for(GateResult::Issues), ISSUES_CONFIDENCE);
        // A clean verdict must claim higher P(clean) than an issues verdict.
        assert!(confidence_for(GateResult::Clean) > confidence_for(GateResult::Issues));
    }

    #[test]
    fn input_maps_the_gate_fields() {
        let input =
            prediction_input(&gate_row("legion-simplify", GateResult::Clean, 0), None).unwrap();
        assert_eq!(input.surface, "legion.gate");
        assert_eq!(input.feature_key, "gate.legion-simplify");
        assert_eq!(input.input_fingerprint, "legion-simplify:deadbeefcafe");
        assert_eq!(input.claimed_confidence.value(), CLEAN_CONFIDENCE);
        assert_eq!(input.model_version, env!("CARGO_PKG_VERSION"));
        assert_eq!(input.prediction_payload["skill"], "legion-simplify");
        assert_eq!(input.prediction_payload["result"], "clean");
        assert!(input.orphan_after.is_some());
    }

    #[test]
    fn emit_inserts_a_retrievable_emitted_prediction() {
        let db = test_db();
        let row = gate_row("legion-simplify", GateResult::Clean, 0);
        let id = emit_gate_prediction(&db, &row, None).unwrap();
        let fetched = db.get_prediction(&id).unwrap().unwrap();
        assert_eq!(fetched.surface, "legion.gate");
        assert_eq!(fetched.state, PredictionState::Emitted);
        assert_eq!(fetched.input_fingerprint, "legion-simplify:deadbeefcafe");
        assert!(fetched.outcome_correctness.is_none());
    }

    #[test]
    fn emit_gate_trust_wrapper_runs_and_emits() {
        // Exercise the non-blocking call-site entry (not just the inner Result
        // fn) and prove it actually emitted: exactly one Emitted legion.gate row
        // afterward (this would fail if the wrapper silently no-op'd). The Err
        // branch is a single non-propagating eprintln -- forcing it needs a
        // corrupted-db fixture and is left as a documented coverage gap, since
        // the branch cannot propagate by construction.
        use crate::uncertainty::types::PredictionState;
        let db = test_db();
        emit_gate_trust(
            &db,
            &gate_row("legion-simplify", GateResult::Clean, 0),
            None,
        );
        let emitted = db
            .count_predictions_by_surface_state("legion.gate", PredictionState::Emitted)
            .unwrap();
        assert_eq!(emitted, 1, "the wrapper must emit exactly one Emitted row");
    }

    #[test]
    fn emit_gate_trust_carries_a_known_issue_and_omits_an_unknown_one() {
        // #1279: a gate recorded for a known issue is findable by that issue
        // (verify's lookup); one recorded with no issue carries none.
        let db = test_db();
        let row = gate_row("legion-pr-write", GateResult::Clean, 0);
        emit_gate_trust(&db, &row, Some("runlegion/legion#1279"));
        emit_gate_trust(&db, &row, None);

        let tagged = db.predictions_for_issue("runlegion/legion#1279").unwrap();
        assert_eq!(tagged.len(), 1);
        assert_eq!(
            tagged[0].issue_ref.as_deref(),
            Some("runlegion/legion#1279")
        );
        assert_eq!(tagged[0].surface, GATE_SURFACE);

        let all = db.list_predictions(Some(GATE_SURFACE), None).unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(all.iter().filter(|p| p.issue_ref.is_none()).count(), 1);
    }

    // -- #780: gate-trust must not ingest asserted-clean for a check-gated
    // skill --------------------------------------------------------------

    #[test]
    fn ungrounded_assertion_detects_asserted_clean_check_gated() {
        let row = gate_row_with_provenance(
            "legion-pr-write",
            GateResult::Clean,
            0,
            GateProvenance::Asserted,
        );
        assert!(is_ungrounded_assertion(&row));
    }

    #[test]
    fn validated_clean_is_not_an_ungrounded_assertion() {
        let row = gate_row_with_provenance(
            "legion-pr-write",
            GateResult::Clean,
            0,
            GateProvenance::Validated,
        );
        assert!(!is_ungrounded_assertion(&row));
    }

    #[test]
    fn asserted_issues_is_not_an_ungrounded_assertion() {
        // Only a CLEAN claim is a ground-truth risk in this direction -- an
        // asserted "issues" result is a self-reported finding, not a
        // manufactured pass.
        let row = gate_row_with_provenance(
            "legion-pr-write",
            GateResult::Issues,
            1,
            GateProvenance::Asserted,
        );
        assert!(!is_ungrounded_assertion(&row));
    }

    #[test]
    fn asserted_clean_for_a_skill_without_a_validator_is_not_ungrounded() {
        // legion-review and a legion-verify:<card_id> verdict have no check
        // validator -- asserted is their only, legitimate provenance, so a
        // clean verdict from them must still be ingested.
        let row = gate_row_with_provenance(
            "legion-review",
            GateResult::Clean,
            0,
            GateProvenance::Asserted,
        );
        assert!(!is_ungrounded_assertion(&row));

        let row = gate_row_with_provenance(
            "legion-verify:card-1",
            GateResult::Clean,
            0,
            GateProvenance::Asserted,
        );
        assert!(!is_ungrounded_assertion(&row));
    }

    #[test]
    fn emit_gate_trust_skips_ingestion_of_an_ungrounded_assertion() {
        use crate::uncertainty::types::PredictionState;
        let db = test_db();
        let row = gate_row_with_provenance(
            "legion-pr-write",
            GateResult::Clean,
            0,
            GateProvenance::Asserted,
        );
        emit_gate_trust(&db, &row, None);
        let emitted = db
            .count_predictions_by_surface_state("legion.gate", PredictionState::Emitted)
            .unwrap();
        assert_eq!(
            emitted, 0,
            "an asserted-clean row for a check-gated skill must not be ingested"
        );
    }

    #[test]
    fn emit_gate_trust_still_ingests_a_validated_clean_row() {
        use crate::uncertainty::types::PredictionState;
        let db = test_db();
        emit_gate_trust(
            &db,
            &gate_row("legion-simplify", GateResult::Clean, 0),
            None,
        );
        let emitted = db
            .count_predictions_by_surface_state("legion.gate", PredictionState::Emitted)
            .unwrap();
        assert_eq!(emitted, 1, "a validated clean row must still be ingested");
    }

    #[test]
    fn issues_verdict_emits_low_confidence() {
        let db = test_db();
        let row = gate_row("legion-review", GateResult::Issues, 3);
        let id = emit_gate_prediction(&db, &row, None).unwrap();
        let fetched = db.get_prediction(&id).unwrap().unwrap();
        assert_eq!(fetched.claimed_confidence.value(), ISSUES_CONFIDENCE);
        assert_eq!(fetched.feature_key, "gate.legion-review");
    }

    // -- #694: decorrelated external witness --------------------------------

    #[test]
    fn external_witness_corroborates_a_clean_verdict() {
        let db = test_db();
        let id = emit_gate_prediction(
            &db,
            &gate_row("legion-simplify", GateResult::Clean, 0),
            None,
        )
        .unwrap();
        let witnessed =
            witness_gate_external(&db, "legion-simplify", "deadbeefcafe", true).unwrap();
        assert_eq!(witnessed, Some(id.clone()));
        let fetched = db.get_prediction(&id).unwrap().unwrap();
        assert_eq!(fetched.state, PredictionState::Witnessed);
        assert_eq!(fetched.outcome_correctness.map(|c| c.value()), Some(1.0));
        assert_eq!(fetched.outcome_label, Some(OutcomeLabel::Shipped));
        assert_eq!(fetched.outcome_payload.unwrap()["witnessed_by"], "operator");
    }

    #[test]
    fn external_witness_marks_a_clean_verdict_wrong() {
        let db = test_db();
        let id = emit_gate_prediction(
            &db,
            &gate_row("legion-simplify", GateResult::Clean, 0),
            None,
        )
        .unwrap();
        let witnessed =
            witness_gate_external(&db, "legion-simplify", "deadbeefcafe", false).unwrap();
        assert_eq!(witnessed, Some(id.clone()));
        let fetched = db.get_prediction(&id).unwrap().unwrap();
        assert_eq!(fetched.state, PredictionState::Witnessed);
        assert_eq!(fetched.outcome_correctness.map(|c| c.value()), Some(0.0));
        assert_eq!(fetched.outcome_label, Some(OutcomeLabel::Escalated));
    }

    #[test]
    fn external_witness_covers_an_issues_verdict_too() {
        // The external witness does NOT restrict to clean verdicts: an operator's ground truth is direct
        // evidence about whichever verdict the gate actually recorded, so an
        // "issues" prediction is just as eligible as a "clean" one.
        //
        // The claimed event is P(clean), so `correct=true` on an ISSUES
        // verdict means the catch was right -- the diff was NOT actually
        // clean -- which must record as Escalated/0.0, not Shipped/1.0.
        let db = test_db();
        let id = emit_gate_prediction(&db, &gate_row("legion-review", GateResult::Issues, 2), None)
            .unwrap();
        let witnessed = witness_gate_external(&db, "legion-review", "deadbeefcafe", true).unwrap();
        assert_eq!(
            witnessed,
            Some(id.clone()),
            "an issues-verdict prediction must be witnessable by the external source"
        );
        let fetched = db.get_prediction(&id).unwrap().unwrap();
        assert_eq!(fetched.outcome_correctness.map(|c| c.value()), Some(0.0));
        assert_eq!(fetched.outcome_label, Some(OutcomeLabel::Escalated));
    }

    #[test]
    fn external_witness_covers_a_false_positive_issues_verdict() {
        // Mirror case: the gate recorded ISSUES but the operator says the
        // catch was wrong (`correct=false`) -- the diff was actually clean.
        // actually_clean = !correct = true, so this must record as
        // Shipped/1.0, not Escalated/0.0.
        let db = test_db();
        let id = emit_gate_prediction(&db, &gate_row("legion-review", GateResult::Issues, 2), None)
            .unwrap();
        let witnessed = witness_gate_external(&db, "legion-review", "deadbeefcafe", false).unwrap();
        assert_eq!(
            witnessed,
            Some(id.clone()),
            "an issues-verdict prediction must be witnessable by the external source"
        );
        let fetched = db.get_prediction(&id).unwrap().unwrap();
        assert_eq!(fetched.outcome_correctness.map(|c| c.value()), Some(1.0));
        assert_eq!(fetched.outcome_label, Some(OutcomeLabel::Shipped));
    }

    #[test]
    fn external_witness_with_no_matching_prediction_is_noop() {
        let db = test_db();
        let witnessed =
            witness_gate_external(&db, "legion-simplify", "nosuchcommit", true).unwrap();
        assert_eq!(
            witnessed, None,
            "expected a no-op when no Emitted prediction matches the fingerprint"
        );
    }

    #[test]
    fn external_witness_takes_the_latest_emitted_on_rerun() {
        let db = test_db();
        let row = gate_row("legion-simplify", GateResult::Clean, 0);
        let _id1 = emit_gate_prediction(&db, &row, None).unwrap();
        let _id2 = emit_gate_prediction(&db, &row, None).unwrap();
        assert!(
            witness_gate_external(&db, "legion-simplify", "deadbeefcafe", true)
                .unwrap()
                .is_some()
        );
        assert!(
            witness_gate_external(&db, "legion-simplify", "deadbeefcafe", true)
                .unwrap()
                .is_some()
        );
        assert_eq!(
            witness_gate_external(&db, "legion-simplify", "deadbeefcafe", true).unwrap(),
            None
        );
    }

    // -- #694: exact-commit undercount is a measured bound -------------------

    #[test]
    fn an_unwitnessed_gate_prediction_surfaces_in_the_orphan_count() {
        // Documents and proves the #694 bound: a clean verdict this module's
        // witnesses miss is never silently dropped -- it stays Emitted until the TTL
        // elapses, then the (external, not-yet-built) sweep orphans it, and
        // `legion uncertainty orphans --surface legion.gate` counts it. This
        // proves the counting path end-to-end for the gate surface
        // specifically, so the undercount is measurable rather than asserted.
        let db = test_db();
        let row = gate_row("legion-simplify", GateResult::Clean, 0);
        let id = emit_gate_prediction(&db, &row, None).unwrap();
        let mut prediction = db.get_prediction(&id).unwrap().unwrap();
        let prev_state = prediction.state;
        prediction.orphan("2026-07-08T00:00:00+00:00").unwrap();
        db.update_prediction(&prediction, prev_state).unwrap();

        let rows = db.count_orphans_by_surface(Some(GATE_SURFACE)).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].surface, GATE_SURFACE);
        assert_eq!(rows[0].count, 1);
    }
}
