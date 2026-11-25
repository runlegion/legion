//! A command legion-cmd constructs is a prediction that it will work,
//! witnessed by whether it did (#1272, FR-CMD-015).
//!
//! When the adapter applies a rewrite, [`emit_rewrite_prediction`] inserts
//! one `legion.cmd` prediction keyed by the tool call's `tool_use_id`, whose
//! payload carries the command as issued and the command constructed. The
//! outcome is read later from the Claude Code session transcript, not from a
//! hook: [`witness_pending`] runs at the start of every adapter run and
//! witnesses each of this session's emitted predictions whose `tool_result`
//! is now in the transcript -- `is_error` true is failed, present without an
//! error is worked, absent is not yet observed and stays emitted (and, past
//! its TTL, orphaned: never counted as worked).
//!
//! A backgrounded call (`run_in_background: true`) is emitted but never
//! witnessed: its transcript result records only that it started.
//!
//! This module writes the prediction and its witness and nothing else: no
//! per-command ledger, credit, or coverage record.

use std::collections::HashSet;
use std::path::Path;

use crate::db::Database;
use crate::uncertainty::error::{Result as UncertaintyResult, UncertaintyError};
use crate::uncertainty::storage::orphan_after_from_ttl;
use crate::uncertainty::types::{
    Confidence, Correctness, OutcomeLabel, Prediction, PredictionInput, UNKNOWN_MODEL,
    model_version_from_id,
};
use crate::usage::{ToolResultOutcome, tool_result_outcomes};

/// The uncertainty surface every legion-cmd rewrite emits under.
pub(crate) const CMD_SURFACE: &str = "legion.cmd";

/// The claim a rewrite makes: the constructed command will work. A fixed
/// placeholder until the spec says where a per-rule confidence comes from;
/// the witnessed rate against it is what the calibrator measures.
const REWRITE_CLAIMED_CONFIDENCE: f64 = 0.9;

/// How long an unwitnessed prediction waits before the orphan sweep takes
/// it. Matches the task-emit hook's TTL.
const CMD_ORPHAN_TTL_DAYS: u32 = 30;

/// The payload key that marks a backgrounded call, written at emit and read
/// by the witness pass to skip it.
const BACKGROUND_KEY: &str = "background";

/// A rewrite the adapter applied, as the prediction records it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AppliedRewrite {
    /// The harness's id for the tool call; the prediction's fingerprint and
    /// the key its `tool_result` is found by. `None` when the payload carried
    /// none, and then nothing can be emitted.
    pub tool_use_id: Option<String>,
    pub session_id: Option<String>,
    pub tool_name: String,
    /// The rewritten field's value as the agent issued it.
    pub issued: Option<String>,
    /// The value legion-cmd constructed in its place.
    pub constructed: String,
    /// The call runs in the background; its result is not an outcome.
    pub background: bool,
}

/// Emits the one prediction for an applied rewrite. `Ok(None)` when the call
/// carried no `tool_use_id`: a prediction nothing could ever witness is not
/// emitted. The model is resolved from the session's statusline samples, the
/// same resolution `legion uncertainty emit --session-id` uses.
pub(crate) fn emit_rewrite_prediction(
    db: &Database,
    rewrite: &AppliedRewrite,
) -> UncertaintyResult<Option<String>> {
    let Some(tool_use_id) = rewrite.tool_use_id.as_deref() else {
        return Ok(None);
    };
    // A failed lookup records the unknown marker, as `legion uncertainty
    // emit` does: the prediction still lands, in a filterable cohort.
    let model: String = rewrite
        .session_id
        .as_deref()
        .and_then(|session_id| match db.latest_model_for_session(session_id) {
            Ok(found) => found,
            Err(e) => {
                eprintln!("[legion cmd-check] model lookup failed: {e}");
                None
            }
        })
        .unwrap_or_else(|| UNKNOWN_MODEL.to_string());
    let input = PredictionInput {
        surface: CMD_SURFACE.to_string(),
        feature_key: format!("rewrite.{}", rewrite.tool_name),
        input_fingerprint: tool_use_id.to_string(),
        model_version: model_version_from_id(&model),
        model,
        claimed_confidence: Confidence::from_f64(REWRITE_CLAIMED_CONFIDENCE)?,
        prediction_payload: serde_json::json!({
            "session_id": rewrite.session_id,
            "tool_name": rewrite.tool_name,
            "issued": rewrite.issued,
            "constructed": rewrite.constructed,
            BACKGROUND_KEY: rewrite.background,
        }),
        orphan_after: orphan_after_from_ttl(CMD_ORPHAN_TTL_DAYS),
        issue_ref: None,
    };
    let prediction = Prediction::new(input);
    db.insert_prediction(&prediction)?;
    Ok(Some(prediction.id))
}

/// Why the witness pass stopped. Every variant leaves the predictions it did
/// not reach emitted; the caller logs it and carries on with its decision.
#[derive(Debug, thiserror::Error)]
pub(crate) enum WitnessPassError {
    #[error("store: {0}")]
    Store(#[from] UncertaintyError),
    #[error("transcript: {0}")]
    Transcript(#[from] std::io::Error),
}

/// Witnesses this session's emitted `legion.cmd` predictions whose
/// `tool_result` is in `transcript`. Returns how many were witnessed.
///
/// Each prediction is re-read by its fingerprint through
/// `latest_emitted_by_fingerprint` and written with `update_prediction`'s
/// compare-and-swap, so a row the orphan sweep moved in between is left
/// alone. A backgrounded call is skipped, and a call with no result yet stays
/// emitted. A failure on one row is reported on stderr and does not stop the
/// rest.
pub(crate) fn witness_pending(
    db: &Database,
    session_id: &str,
    transcript: &Path,
) -> Result<usize, WitnessPassError> {
    let pending: HashSet<String> = db
        .emitted_for_session(CMD_SURFACE, session_id)?
        .into_iter()
        .filter(|p| !is_background(p))
        .map(|p| p.input_fingerprint)
        .collect();
    if pending.is_empty() {
        return Ok(0);
    }
    let outcomes = tool_result_outcomes(transcript, &pending)?;
    let mut witnessed: usize = 0;
    for (tool_use_id, outcome) in outcomes {
        match witness_one(db, &tool_use_id, outcome) {
            Ok(true) => witnessed += 1,
            Ok(false) => {}
            Err(e) => eprintln!("[legion cmd-check] witness of {tool_use_id} failed: {e}"),
        }
    }
    Ok(witnessed)
}

fn is_background(prediction: &Prediction) -> bool {
    prediction
        .prediction_payload
        .get(BACKGROUND_KEY)
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false)
}

/// Witnesses the latest emitted prediction for `tool_use_id` with its
/// transcript outcome. `Ok(false)` when there is none left to witness.
fn witness_one(
    db: &Database,
    tool_use_id: &str,
    outcome: ToolResultOutcome,
) -> UncertaintyResult<bool> {
    let Some(mut prediction) = db.latest_emitted_by_fingerprint(CMD_SURFACE, tool_use_id)? else {
        return Ok(false);
    };
    let (label, correctness, worked) = match outcome {
        ToolResultOutcome::Worked => (OutcomeLabel::Shipped, 1.0, true),
        ToolResultOutcome::Failed => (OutcomeLabel::Abandoned, 0.0, false),
    };
    let payload = serde_json::json!({
        "witnessed_by": "transcript",
        "worked": worked,
    });
    let now = chrono::Utc::now().to_rfc3339();
    // Captured before the in-memory transition so the write is a CAS against
    // the orphan sweep (#1003).
    let prev_state = prediction.state;
    prediction.witness(label, payload, Correctness::from_f64(correctness)?, &now)?;
    db.update_prediction(&prediction, prev_state)?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::testutil::test_db;
    use crate::uncertainty::types::PredictionState;
    use std::io::Write;

    fn rewrite(tool_use_id: &str, session_id: &str, background: bool) -> AppliedRewrite {
        AppliedRewrite {
            tool_use_id: Some(tool_use_id.to_string()),
            session_id: Some(session_id.to_string()),
            tool_name: "Bash".to_string(),
            issued: Some("gh issue list".to_string()),
            constructed: "legion issue list".to_string(),
            background,
        }
    }

    fn transcript(lines: &[String]) -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("session.jsonl");
        let mut file = std::fs::File::create(&path).expect("create");
        for line in lines {
            writeln!(file, "{line}").expect("write");
        }
        (dir, path)
    }

    fn result_line(id: &str, is_error: bool) -> String {
        format!(
            r#"{{"type":"user","message":{{"role":"user","content":[{{"type":"tool_result","tool_use_id":"{id}","is_error":{is_error},"content":"x"}}]}}}}"#
        )
    }

    fn only(db: &Database) -> Prediction {
        let all = db.list_predictions(Some(CMD_SURFACE), None).expect("list");
        assert_eq!(all.len(), 1, "expected exactly one legion.cmd prediction");
        all.into_iter().next().expect("one")
    }

    #[test]
    fn a_rewrite_emits_one_prediction_carrying_issued_and_constructed() {
        let db = test_db();
        let id = emit_rewrite_prediction(&db, &rewrite("toolu_1", "s1", false))
            .expect("emits")
            .expect("has an id");
        let p = only(&db);
        assert_eq!(p.id, id);
        assert_eq!(p.surface, CMD_SURFACE);
        assert_eq!(p.input_fingerprint, "toolu_1");
        assert_eq!(p.state, PredictionState::Emitted);
        assert_eq!(p.prediction_payload["issued"], "gh issue list");
        assert_eq!(p.prediction_payload["constructed"], "legion issue list");
        assert_eq!(p.prediction_payload["session_id"], "s1");
        assert!(p.orphan_after.is_some());
    }

    #[test]
    fn a_rewrite_without_a_tool_use_id_emits_nothing() {
        let db = test_db();
        let mut applied = rewrite("unused", "s1", false);
        applied.tool_use_id = None;
        assert_eq!(emit_rewrite_prediction(&db, &applied).expect("ok"), None);
        assert!(
            db.list_predictions(Some(CMD_SURFACE), None)
                .expect("list")
                .is_empty()
        );
    }

    #[test]
    fn a_tool_result_without_error_witnesses_worked() {
        let db = test_db();
        emit_rewrite_prediction(&db, &rewrite("toolu_ok", "s1", false)).expect("emits");
        let (_dir, path) = transcript(&[result_line("toolu_ok", false)]);
        assert_eq!(witness_pending(&db, "s1", &path).expect("pass"), 1);
        let p = only(&db);
        assert_eq!(p.state, PredictionState::Witnessed);
        assert_eq!(p.outcome_label, Some(OutcomeLabel::Shipped));
        assert_eq!(p.outcome_correctness.map(|c| c.value()), Some(1.0));
        assert_eq!(p.outcome_payload.expect("payload")["worked"], true);
    }

    #[test]
    fn an_error_tool_result_witnesses_failed() {
        let db = test_db();
        emit_rewrite_prediction(&db, &rewrite("toolu_bad", "s1", false)).expect("emits");
        let (_dir, path) = transcript(&[result_line("toolu_bad", true)]);
        assert_eq!(witness_pending(&db, "s1", &path).expect("pass"), 1);
        let p = only(&db);
        assert_eq!(p.state, PredictionState::Witnessed);
        assert_eq!(p.outcome_label, Some(OutcomeLabel::Abandoned));
        assert_eq!(p.outcome_correctness.map(|c| c.value()), Some(0.0));
        assert_eq!(p.outcome_payload.expect("payload")["worked"], false);
    }

    #[test]
    fn a_call_with_no_result_yet_stays_emitted() {
        let db = test_db();
        emit_rewrite_prediction(&db, &rewrite("toolu_pending", "s1", false)).expect("emits");
        let (_dir, path) = transcript(&[result_line("toolu_someone_else", false)]);
        assert_eq!(witness_pending(&db, "s1", &path).expect("pass"), 0);
        let p = only(&db);
        assert_eq!(p.state, PredictionState::Emitted);
        assert!(p.outcome_label.is_none());
    }

    #[test]
    fn a_backgrounded_rewrite_is_never_witnessed() {
        let db = test_db();
        emit_rewrite_prediction(&db, &rewrite("toolu_bg", "s1", true)).expect("emits");
        let (_dir, path) = transcript(&[result_line("toolu_bg", false)]);
        assert_eq!(witness_pending(&db, "s1", &path).expect("pass"), 0);
        assert_eq!(only(&db).state, PredictionState::Emitted);
    }

    #[test]
    fn the_pass_witnesses_only_its_own_session() {
        let db = test_db();
        emit_rewrite_prediction(&db, &rewrite("toolu_other", "s2", false)).expect("emits");
        let (_dir, path) = transcript(&[result_line("toolu_other", false)]);
        assert_eq!(witness_pending(&db, "s1", &path).expect("pass"), 0);
        assert_eq!(only(&db).state, PredictionState::Emitted);
    }

    #[test]
    fn an_unreadable_transcript_fails_the_pass_and_leaves_it_emitted() {
        let db = test_db();
        emit_rewrite_prediction(&db, &rewrite("toolu_1", "s1", false)).expect("emits");
        let err = witness_pending(&db, "s1", Path::new("/nonexistent/session.jsonl"));
        assert!(matches!(err, Err(WitnessPassError::Transcript(_))));
        assert_eq!(only(&db).state, PredictionState::Emitted);
    }

    #[test]
    fn an_orphaned_prediction_is_not_witnessed_late() {
        let db = test_db();
        emit_rewrite_prediction(&db, &rewrite("toolu_old", "s1", false)).expect("emits");
        // Past every orphan_after: the sweep takes the unwitnessed row.
        assert_eq!(
            db.sweep_orphans("9999-01-01T00:00:00+00:00")
                .expect("sweep"),
            1
        );
        let (_dir, path) = transcript(&[result_line("toolu_old", false)]);
        assert_eq!(witness_pending(&db, "s1", &path).expect("pass"), 0);
        let p = only(&db);
        assert_eq!(p.state, PredictionState::Orphaned);
        assert!(p.outcome_correctness.is_none());
    }

    #[test]
    fn a_second_pass_does_not_witness_twice() {
        let db = test_db();
        emit_rewrite_prediction(&db, &rewrite("toolu_ok", "s1", false)).expect("emits");
        let (_dir, path) = transcript(&[result_line("toolu_ok", false)]);
        assert_eq!(witness_pending(&db, "s1", &path).expect("pass"), 1);
        assert_eq!(witness_pending(&db, "s1", &path).expect("pass"), 0);
        assert_eq!(only(&db).state, PredictionState::Witnessed);
    }
}
