//! Pillar 2: uncertainty engine.
//!
//! Turns features from SCIP + task descriptions into a calibrated
//! prediction (predicted_tokens, predicted_duration, predicted_review_rounds),
//! witnesses the outcome when work completes, and rolls reliability
//! snapshots so vault's COS routing has live cost estimates.
//!
//! This module owns the domain types and lifecycle math. Database CRUD
//! lands in `db.rs` alongside the rest of the storage layer; the hookable
//! CLI surface (`legion uncertainty ...`) wires the two together in
//! `main.rs` (see issue #357).
//!
//! Module layout:
//!
//! - [`types`] -- Prediction, CalibrationSnapshot, PredictionState,
//!   OutcomeLabel, Confidence, PredictionInput, cohort_key helper.
//! - [`error`] -- UncertaintyError via thiserror.

pub mod error;
pub mod types;

// Re-exports land here once #357 (CLI) and #358 (hooks) start consuming
// the types. Keeping them un-re-exported now keeps `cargo clippy -D warnings`
// quiet without sprinkling per-item allow attributes that would have to be
// reverted later.
