//! Domain types for the uncertainty engine.
//!
//! Mirrors the platform schema (rafters-studio/platform docs/designs/uncertainty-engine.md)
//! in idiomatic Rust. Where platform uses zod runtime validation, legion
//! uses the type system + serde at the boundary.

// The module ships ahead of its consumers: the CLI surface (#357) and
// the auto-emit/witness hooks (#358) call these constructors and state
// transitions. dead_code warnings are intentional non-noise until those
// land.
#![allow(dead_code)]
//!
//! Lifecycle:
//!
//! ```text
//!                  witness            calibrate
//!  Emitted ----------------> Witnessed ---------> Calibrated
//!     \                                                 \
//!      \                                                 \
//!       orphan_after                                      retire
//!           \                                              \
//!            \---> Orphaned -----> Retired                 Retired
//! ```
//!
//! Orphaned is a terminal-ish state -- it can only retire from there.
//! It cannot back-track to Witnessed even if outcome data arrives late;
//! that prevents the orphan-sweep from racing the witness hook.

use serde::{Deserialize, Serialize};

use super::error::{Result, UncertaintyError};

/// Lifecycle states for a prediction row.
///
/// String values match the column literals so [`PredictionState::as_str`] and
/// [`PredictionState::from_str`] round-trip cleanly through the database.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PredictionState {
    Emitted,
    Witnessed,
    Calibrated,
    Orphaned,
    Retired,
}

impl PredictionState {
    /// Storage form. Matches the `state` column literal.
    pub fn as_str(&self) -> &'static str {
        match self {
            PredictionState::Emitted => "emitted",
            PredictionState::Witnessed => "witnessed",
            PredictionState::Calibrated => "calibrated",
            PredictionState::Orphaned => "orphaned",
            PredictionState::Retired => "retired",
        }
    }

    /// Parse a storage-form state literal back into the enum.
    pub fn from_str(s: &str) -> Result<Self> {
        match s {
            "emitted" => Ok(PredictionState::Emitted),
            "witnessed" => Ok(PredictionState::Witnessed),
            "calibrated" => Ok(PredictionState::Calibrated),
            "orphaned" => Ok(PredictionState::Orphaned),
            "retired" => Ok(PredictionState::Retired),
            other => Err(UncertaintyError::InvalidPayload(format!(
                "unknown prediction state: {other}"
            ))),
        }
    }

    /// Check whether a transition is allowed by the lifecycle.
    ///
    /// Caller-friendly companion to [`PredictionState::transition`]: returns
    /// a bool when you want to gate behavior without producing an error.
    pub fn can_transition_to(&self, next: PredictionState) -> bool {
        matches!(
            (self, next),
            (PredictionState::Emitted, PredictionState::Witnessed)
                | (PredictionState::Emitted, PredictionState::Orphaned)
                | (PredictionState::Witnessed, PredictionState::Calibrated)
                | (PredictionState::Witnessed, PredictionState::Retired)
                | (PredictionState::Calibrated, PredictionState::Retired)
                | (PredictionState::Orphaned, PredictionState::Retired)
        )
    }

    /// Attempt the transition, returning the new state or an error
    /// describing the rejected move.
    pub fn transition(self, next: PredictionState) -> Result<PredictionState> {
        if self.can_transition_to(next) {
            Ok(next)
        } else {
            Err(UncertaintyError::IllegalTransition {
                from: self,
                to: next,
            })
        }
    }
}

/// Witness outcome categories.
///
/// `Shipped` is the happy path (predicted + actual within tolerance).
/// `ScopedDown` and `Escalated` are deliberate course corrections that
/// still produce signal. `Abandoned` is the negative outcome the
/// calibrator needs to track to avoid over-confident estimates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum OutcomeLabel {
    Shipped,
    ScopedDown,
    Escalated,
    Abandoned,
}

impl OutcomeLabel {
    pub fn as_str(&self) -> &'static str {
        match self {
            OutcomeLabel::Shipped => "shipped",
            OutcomeLabel::ScopedDown => "scoped-down",
            OutcomeLabel::Escalated => "escalated",
            OutcomeLabel::Abandoned => "abandoned",
        }
    }

    pub fn from_str(s: &str) -> Result<Self> {
        match s {
            "shipped" => Ok(OutcomeLabel::Shipped),
            "scoped-down" => Ok(OutcomeLabel::ScopedDown),
            "escalated" => Ok(OutcomeLabel::Escalated),
            "abandoned" => Ok(OutcomeLabel::Abandoned),
            other => Err(UncertaintyError::InvalidPayload(format!(
                "unknown outcome label: {other}"
            ))),
        }
    }
}

/// Probability-shaped confidence in [0.0, 1.0].
///
/// Wrapped at the type boundary so callers cannot construct an out-of-range
/// value silently. The constructor validates; serde uses
/// [`Confidence::from_f64`] via try_into to keep round-trips honest.
///
/// `PartialEq` is safe to derive: the constructor rejects NaN, so no
/// instance of `Confidence` can compare unequal to itself (the trait
/// invariant `a == a`).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "f64", into = "f64")]
pub struct Confidence(f64);

impl Confidence {
    /// Construct from a raw f64. Returns `InvalidConfidence` on out-of-range.
    pub fn from_f64(value: f64) -> Result<Self> {
        if !value.is_finite() || !(0.0..=1.0).contains(&value) {
            return Err(UncertaintyError::InvalidConfidence(value));
        }
        Ok(Confidence(value))
    }

    pub fn value(&self) -> f64 {
        self.0
    }
}

impl TryFrom<f64> for Confidence {
    type Error = UncertaintyError;
    fn try_from(value: f64) -> Result<Self> {
        Confidence::from_f64(value)
    }
}

impl From<Confidence> for f64 {
    fn from(c: Confidence) -> f64 {
        c.0
    }
}

/// Witness-side companion to [`Confidence`]. Measures actual outcome
/// closeness to the prediction, normalized to [0.0, 1.0].
///
/// The witness path needs the same [0, 1] guarantee `Confidence` gives
/// the emit path. Same NaN-rejection, same `PartialEq` safety argument.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "f64", into = "f64")]
pub struct Correctness(f64);

impl Correctness {
    pub fn from_f64(value: f64) -> Result<Self> {
        if !value.is_finite() || !(0.0..=1.0).contains(&value) {
            return Err(UncertaintyError::InvalidCorrectness(value));
        }
        Ok(Correctness(value))
    }

    pub fn value(&self) -> f64 {
        self.0
    }
}

impl TryFrom<f64> for Correctness {
    type Error = UncertaintyError;
    fn try_from(value: f64) -> Result<Self> {
        Correctness::from_f64(value)
    }
}

impl From<Correctness> for f64 {
    fn from(c: Correctness) -> f64 {
        c.0
    }
}

/// Inputs to an emit call.
///
/// Captures the (surface, feature_key, model, fingerprint) tuple that
/// identifies a prediction context plus the prediction payload itself.
/// The cohort_key is derived rather than passed: it is a deterministic
/// function of (surface, model, model_version, claimed_confidence
/// bucket) so two callers producing the same prediction context end
/// up in the same cohort.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PredictionInput {
    pub surface: String,
    pub feature_key: String,
    pub input_fingerprint: String,
    pub model: String,
    pub model_version: String,
    pub claimed_confidence: Confidence,
    pub prediction_payload: serde_json::Value,
    pub orphan_after: Option<String>,
}

/// A row of `uncertainty_prediction`.
///
/// Mirrors the storage layout. Construction goes through [`Prediction::new`]
/// from a [`PredictionInput`] so callers cannot accidentally bypass the
/// id-and-timestamp generation, or set `state` to something other than
/// `Emitted` at birth.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Prediction {
    pub id: String,
    pub surface: String,
    pub feature_key: String,
    pub input_fingerprint: String,
    pub model: String,
    pub model_version: String,
    pub claimed_confidence: Confidence,
    pub prediction_payload: serde_json::Value,
    pub state: PredictionState,
    pub outcome_label: Option<OutcomeLabel>,
    pub outcome_payload: Option<serde_json::Value>,
    pub outcome_correctness: Option<f64>,
    pub cohort_key: String,
    pub created_at: String,
    pub updated_at: String,
    pub witnessed_at: Option<String>,
    pub orphan_after: Option<String>,
}

impl Prediction {
    /// Build a freshly-emitted prediction. Assigns a UUIDv7 id and ISO 8601
    /// timestamps. Derives the cohort_key from (surface, model,
    /// model_version, confidence-bucket); callers do not pass it.
    pub fn new(input: PredictionInput) -> Self {
        let id = uuid::Uuid::now_v7().to_string();
        let now = chrono::Utc::now().to_rfc3339();
        let cohort_key = cohort_key(
            &input.surface,
            &input.model,
            &input.model_version,
            input.claimed_confidence,
        );
        Self {
            id,
            surface: input.surface,
            feature_key: input.feature_key,
            input_fingerprint: input.input_fingerprint,
            model: input.model,
            model_version: input.model_version,
            claimed_confidence: input.claimed_confidence,
            prediction_payload: input.prediction_payload,
            state: PredictionState::Emitted,
            outcome_label: None,
            outcome_payload: None,
            outcome_correctness: None,
            cohort_key,
            created_at: now.clone(),
            updated_at: now,
            witnessed_at: None,
            orphan_after: input.orphan_after,
        }
    }

    /// Apply witness data. Returns an error if the current state forbids it.
    ///
    /// `correctness` is wrapped in [`Correctness`] so the witness path
    /// shares the same validated [0, 1] guarantee the emit path gets from
    /// [`Confidence`].
    pub fn witness(
        &mut self,
        label: OutcomeLabel,
        payload: serde_json::Value,
        correctness: Correctness,
        now: &str,
    ) -> Result<()> {
        self.state = self.state.transition(PredictionState::Witnessed)?;
        self.outcome_label = Some(label);
        self.outcome_payload = Some(payload);
        self.outcome_correctness = Some(correctness.value());
        self.witnessed_at = Some(now.to_owned());
        self.updated_at = now.to_owned();
        Ok(())
    }

    /// Move a witnessed prediction into the calibrated state.
    pub fn calibrate(&mut self, now: &str) -> Result<()> {
        self.state = self.state.transition(PredictionState::Calibrated)?;
        self.updated_at = now.to_owned();
        Ok(())
    }

    /// Move an emitted prediction into the orphan state.
    ///
    /// Callers (the hourly sweep) set this when `orphan_after` has elapsed
    /// without a witness. Refuses on any other current state so a late
    /// witness cannot be retroactively reclassified.
    pub fn orphan(&mut self, now: &str) -> Result<()> {
        self.state = self.state.transition(PredictionState::Orphaned)?;
        self.updated_at = now.to_owned();
        Ok(())
    }

    /// Retire a witnessed, calibrated, or orphaned prediction. Terminal.
    pub fn retire(&mut self, now: &str) -> Result<()> {
        self.state = self.state.transition(PredictionState::Retired)?;
        self.updated_at = now.to_owned();
        Ok(())
    }
}

/// A row of `uncertainty_calibration_snapshot`.
///
/// `actual_correctness` is the Empirical-Bayes shrunk value used for
/// reliability math; `actual_correctness_raw` is the unshrunk cell average
/// retained for audit. The bucket bounds carry quantile-derived bin edges
/// (equal-frequency, 10 per cohort) per the post-correction notes from
/// platform's review.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CalibrationSnapshot {
    pub id: String,
    pub cohort_key: String,
    pub bucket_lower: f64,
    pub bucket_upper: f64,
    pub claimed_confidence: f64,
    pub actual_correctness: f64,
    pub actual_correctness_raw: f64,
    pub prediction_count: i64,
    pub orphan_count: i64,
    pub brier_score: f64,
    pub computed_at: String,
    pub updated_at: String,
}

/// Map a confidence value into one of ten bucket labels: 00..09.
///
/// Equal-width buckets are used here for cohort_key derivation only --
/// the calibration snapshots use quantile-derived edges instead. Two
/// different bucketing schemes by design: cohort_key needs to be
/// deterministic from inputs alone (no data lookup), while reliability
/// buckets need equal frequency to avoid sparse-corner bias.
fn confidence_bucket_label(c: Confidence) -> String {
    let value = c.value();
    let idx = ((value * 10.0).floor() as i32).clamp(0, 9);
    format!("{:02}", idx)
}

/// Deterministic cohort key string. Format mirrors platform:
/// `<surface>:<model>:<model_version>:<bucket>`.
pub fn cohort_key(surface: &str, model: &str, model_version: &str, c: Confidence) -> String {
    format!(
        "{}:{}:{}:{}",
        surface,
        model,
        model_version,
        confidence_bucket_label(c)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fresh_input() -> PredictionInput {
        PredictionInput {
            surface: "legion.task".into(),
            feature_key: "scip.refactor".into(),
            input_fingerprint: "fp-1".into(),
            model: "claude-opus-4-7".into(),
            model_version: "4.7".into(),
            claimed_confidence: Confidence::from_f64(0.72).unwrap(),
            prediction_payload: serde_json::json!({ "predicted_tokens": 1500 }),
            orphan_after: None,
        }
    }

    #[test]
    fn confidence_rejects_out_of_range() {
        assert!(Confidence::from_f64(-0.1).is_err());
        assert!(Confidence::from_f64(1.1).is_err());
        assert!(Confidence::from_f64(f64::NAN).is_err());
        assert!(Confidence::from_f64(f64::INFINITY).is_err());
        assert!(Confidence::from_f64(0.0).is_ok());
        assert!(Confidence::from_f64(1.0).is_ok());
    }

    #[test]
    fn state_round_trip_through_str() {
        for state in [
            PredictionState::Emitted,
            PredictionState::Witnessed,
            PredictionState::Calibrated,
            PredictionState::Orphaned,
            PredictionState::Retired,
        ] {
            assert_eq!(PredictionState::from_str(state.as_str()).unwrap(), state);
        }
    }

    #[test]
    fn happy_path_emit_witness_calibrate_retire() {
        let mut p = Prediction::new(fresh_input());
        assert_eq!(p.state, PredictionState::Emitted);
        p.witness(
            OutcomeLabel::Shipped,
            serde_json::json!({ "actual_tokens": 1480 }),
            Correctness::from_f64(0.98).unwrap(),
            "2026-05-12T10:00:00+00:00",
        )
        .unwrap();
        assert_eq!(p.state, PredictionState::Witnessed);
        assert_eq!(p.outcome_label, Some(OutcomeLabel::Shipped));
        p.calibrate("2026-05-13T03:00:00+00:00").unwrap();
        assert_eq!(p.state, PredictionState::Calibrated);
        p.retire("2026-05-20T00:00:00+00:00").unwrap();
        assert_eq!(p.state, PredictionState::Retired);
    }

    #[test]
    fn orphan_path_blocks_late_witness() {
        let mut p = Prediction::new(fresh_input());
        p.orphan("2026-05-12T11:00:00+00:00").unwrap();
        assert_eq!(p.state, PredictionState::Orphaned);
        let err = p
            .witness(
                OutcomeLabel::Shipped,
                serde_json::json!({}),
                Correctness::from_f64(1.0).unwrap(),
                "2026-05-12T12:00:00+00:00",
            )
            .unwrap_err();
        assert!(matches!(
            err,
            UncertaintyError::IllegalTransition {
                from: PredictionState::Orphaned,
                to: PredictionState::Witnessed,
            }
        ));
    }

    #[test]
    fn cannot_retire_an_emitted_prediction() {
        let mut p = Prediction::new(fresh_input());
        let err = p.retire("2026-05-12T11:00:00+00:00").unwrap_err();
        assert!(matches!(
            err,
            UncertaintyError::IllegalTransition {
                from: PredictionState::Emitted,
                to: PredictionState::Retired,
            }
        ));
    }

    #[test]
    fn cannot_calibrate_without_witness() {
        let mut p = Prediction::new(fresh_input());
        let err = p.calibrate("2026-05-12T11:00:00+00:00").unwrap_err();
        assert!(matches!(
            err,
            UncertaintyError::IllegalTransition {
                from: PredictionState::Emitted,
                to: PredictionState::Calibrated,
            }
        ));
    }

    #[test]
    fn cannot_double_witness() {
        let mut p = Prediction::new(fresh_input());
        p.witness(
            OutcomeLabel::Shipped,
            serde_json::json!({}),
            Correctness::from_f64(1.0).unwrap(),
            "2026-05-12T10:00:00+00:00",
        )
        .unwrap();
        let err = p
            .witness(
                OutcomeLabel::Abandoned,
                serde_json::json!({}),
                Correctness::from_f64(0.0).unwrap(),
                "2026-05-12T11:00:00+00:00",
            )
            .unwrap_err();
        assert!(matches!(
            err,
            UncertaintyError::IllegalTransition {
                from: PredictionState::Witnessed,
                to: PredictionState::Witnessed,
            }
        ));
    }

    #[test]
    fn orphan_can_retire() {
        let mut p = Prediction::new(fresh_input());
        p.orphan("2026-05-12T11:00:00+00:00").unwrap();
        p.retire("2026-05-19T00:00:00+00:00").unwrap();
        assert_eq!(p.state, PredictionState::Retired);
    }

    #[test]
    fn cohort_key_is_deterministic() {
        let c = Confidence::from_f64(0.73).unwrap();
        let key = cohort_key("legion.task", "claude-opus-4-7", "4.7", c);
        assert_eq!(key, "legion.task:claude-opus-4-7:4.7:07");
    }

    #[test]
    fn confidence_bucket_pins_boundaries() {
        assert_eq!(
            confidence_bucket_label(Confidence::from_f64(0.0).unwrap()),
            "00"
        );
        assert_eq!(
            confidence_bucket_label(Confidence::from_f64(0.1).unwrap()),
            "01"
        );
        assert_eq!(
            confidence_bucket_label(Confidence::from_f64(0.999).unwrap()),
            "09"
        );
        assert_eq!(
            confidence_bucket_label(Confidence::from_f64(1.0).unwrap()),
            "09"
        );
    }

    #[test]
    fn outcome_label_round_trips() {
        for label in [
            OutcomeLabel::Shipped,
            OutcomeLabel::ScopedDown,
            OutcomeLabel::Escalated,
            OutcomeLabel::Abandoned,
        ] {
            assert_eq!(OutcomeLabel::from_str(label.as_str()).unwrap(), label);
        }
    }

    #[test]
    fn new_prediction_starts_emitted_with_uuidv7() {
        let p = Prediction::new(fresh_input());
        assert_eq!(p.state, PredictionState::Emitted);
        assert!(p.outcome_label.is_none());
        assert!(p.witnessed_at.is_none());
        let parsed = uuid::Uuid::parse_str(&p.id).unwrap();
        assert_eq!(parsed.get_version_num(), 7);
    }

    #[test]
    fn witnessed_can_retire_without_calibrating() {
        let mut p = Prediction::new(fresh_input());
        p.witness(
            OutcomeLabel::Shipped,
            serde_json::json!({}),
            Correctness::from_f64(0.9).unwrap(),
            "2026-05-12T10:00:00+00:00",
        )
        .unwrap();
        p.retire("2026-05-13T00:00:00+00:00").unwrap();
        assert_eq!(p.state, PredictionState::Retired);
    }

    #[test]
    fn confidence_serde_round_trip_via_json() {
        let c = Confidence::from_f64(0.42).unwrap();
        let encoded = serde_json::to_string(&c).unwrap();
        assert_eq!(encoded, "0.42");
        let decoded: Confidence = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded, c);
        let bad: serde_json::Result<Confidence> = serde_json::from_str("1.5");
        assert!(bad.is_err());
    }

    #[test]
    fn correctness_serde_round_trip_via_json() {
        let c = Correctness::from_f64(0.66).unwrap();
        let encoded = serde_json::to_string(&c).unwrap();
        assert_eq!(encoded, "0.66");
        let decoded: Correctness = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded, c);
        let bad: serde_json::Result<Correctness> = serde_json::from_str("-0.1");
        assert!(bad.is_err());
    }

    #[test]
    fn state_from_str_rejects_unknown_label() {
        let err = PredictionState::from_str("emerging").unwrap_err();
        assert!(matches!(err, UncertaintyError::InvalidPayload(_)));
    }

    #[test]
    fn outcome_label_from_str_rejects_unknown_label() {
        let err = OutcomeLabel::from_str("ghosted").unwrap_err();
        assert!(matches!(err, UncertaintyError::InvalidPayload(_)));
    }
}
