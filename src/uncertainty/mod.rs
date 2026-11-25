//! Pillar 2: uncertainty engine.
//!
//! Turns features from SCIP + task descriptions into a calibrated
//! prediction (predicted_tokens, predicted_duration, predicted_review_rounds),
//! witnesses the outcome when work completes, and rolls reliability
//! snapshots so vault's COS routing has live cost estimates.
//!
//! This module owns the domain types, lifecycle math, and storage CRUD
//! for predictions and calibration snapshots. The CLI surface
//! (`legion uncertainty ...`) wires the pieces together in `main.rs`.
//!
//! Module layout:
//!
//! - [`types`] -- Prediction, CalibrationSnapshot, PredictionState,
//!   OutcomeLabel, Confidence, Correctness, PredictionInput, cohort_key helper.
//! - [`error`] -- UncertaintyError via thiserror.
//! - [`storage`] -- impl Database for insert/get/update/list + the
//!   `orphan_after_from_ttl` helper used at the CLI emit boundary.

pub mod error;
pub mod storage;
pub mod types;
