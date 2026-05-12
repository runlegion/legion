//! Error type for the uncertainty engine.
//!
//! Mirrors the rest of legion: a single enum via `thiserror`, with
//! variants for the failure modes callers need to discriminate on
//! (illegal state transitions, payload validation, database errors).
//! Database and serde failures wrap their source so the original
//! error chain stays inspectable.

// Variants ship ahead of their callers (#357 CLI, #358 hooks).
#![allow(dead_code)]

use thiserror::Error;

use super::types::PredictionState;

/// Result alias used throughout the uncertainty module.
pub type Result<T> = std::result::Result<T, UncertaintyError>;

/// Failure modes for the uncertainty engine.
#[derive(Debug, Error)]
pub enum UncertaintyError {
    /// A state transition was requested that the lifecycle forbids.
    ///
    /// Examples: witnessing an orphaned prediction, retiring an emitted
    /// prediction without first witnessing it, calibrating a row that
    /// has not been witnessed.
    #[error("illegal transition: cannot move from {from:?} to {to:?}")]
    IllegalTransition {
        from: PredictionState,
        to: PredictionState,
    },

    /// A confidence value fell outside the [0.0, 1.0] interval. Predictions
    /// are probability-shaped; out-of-range inputs are caller bugs.
    #[error("claimed_confidence must be in [0.0, 1.0], got {0}")]
    InvalidConfidence(f64),

    /// A correctness value fell outside the [0.0, 1.0] interval.
    #[error("outcome_correctness must be in [0.0, 1.0], got {0}")]
    InvalidCorrectness(f64),

    /// A prediction payload failed JSON validation.
    #[error("prediction payload invalid: {0}")]
    InvalidPayload(String),

    /// Lookup by id returned no row.
    #[error("prediction not found: {0}")]
    PredictionNotFound(String),

    /// Underlying database error.
    #[error("database error: {0}")]
    Database(#[from] rusqlite::Error),

    /// JSON serialization failure (payload encode/decode).
    #[error("serde error: {0}")]
    Serde(#[from] serde_json::Error),
}
