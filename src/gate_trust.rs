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

use crate::db::Database;
use crate::db::quality_gates::QualityGateRow;
use crate::uncertainty::error::Result as UncertaintyResult;
use crate::uncertainty::storage::orphan_after_from_ttl;
use crate::uncertainty::types::{Confidence, Prediction, PredictionInput};
use crate::verify::GateResult;

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
/// verdict, payload carrying the gate context.
fn prediction_input(row: &QualityGateRow) -> UncertaintyResult<PredictionInput> {
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
    })
}

/// Emit a gate verdict as an uncertainty prediction. Returns the prediction id
/// on success. The non-blocking wrapper `emit_gate_trust` is what call sites
/// use; this inner function returns the Result for tests.
pub fn emit_gate_prediction(db: &Database, row: &QualityGateRow) -> UncertaintyResult<String> {
    let prediction = Prediction::new(prediction_input(row)?);
    db.insert_prediction(&prediction)?;
    Ok(prediction.id)
}

/// Non-blocking gate-trust emit for the gate-record handlers: log on failure,
/// never propagate. A gate-trust problem must never break gate recording or the
/// agent's workflow -- the measurement is best-effort, the gate is not.
pub fn emit_gate_trust(db: &Database, row: &QualityGateRow) {
    if let Err(e) = emit_gate_prediction(db, row) {
        eprintln!("[legion] gate-trust emit failed (non-fatal): {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::testutil::test_db;
    use crate::uncertainty::types::PredictionState;

    fn gate_row(skill: &str, result: GateResult, findings: u64) -> QualityGateRow {
        QualityGateRow {
            id: "gate-id".to_string(),
            branch: "feat/x".to_string(),
            commit_hash: "deadbeefcafe".to_string(),
            skill: skill.to_string(),
            result,
            findings_count: findings,
            details: None,
            created_at: "2026-06-27T00:00:00+00:00".to_string(),
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
        let input = prediction_input(&gate_row("legion-simplify", GateResult::Clean, 0)).unwrap();
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
        let id = emit_gate_prediction(&db, &row).unwrap();
        let fetched = db.get_prediction(&id).unwrap().unwrap();
        assert_eq!(fetched.surface, "legion.gate");
        assert_eq!(fetched.state, PredictionState::Emitted);
        assert_eq!(fetched.input_fingerprint, "legion-simplify:deadbeefcafe");
        assert!(fetched.outcome_correctness.is_none());
    }

    #[test]
    fn issues_verdict_emits_low_confidence() {
        let db = test_db();
        let row = gate_row("legion-review", GateResult::Issues, 3);
        let id = emit_gate_prediction(&db, &row).unwrap();
        let fetched = db.get_prediction(&id).unwrap().unwrap();
        assert_eq!(fetched.claimed_confidence.value(), ISSUES_CONFIDENCE);
        assert_eq!(fetched.feature_key, "gate.legion-review");
    }
}
