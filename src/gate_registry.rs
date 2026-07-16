//! Validator registry (#780): the single source of truth for which
//! quality-gate skills have a CHECK validator (a structural
//! coverage-plus-substance gate the skill's articulation must pass before a
//! row is recorded) versus which are asserted-by-necessity (no validator
//! exists, so `quality-gate record` is the only way to write their verdict --
//! `legion-review`, and a card-keyed `legion-verify:<card_id>` verdict).
//!
//! Before this module, "which skills have a check validator" was implicit:
//! `legion-simplify` and `legion-pr-write` were hardcoded, separately, in
//! `cli/pr.rs`'s pre-create gate loop and `cli/verify.rs`'s `Check` action,
//! with nothing tying them together. Two independent call sites now need the
//! same list to stay in lockstep -- `quality-gate record`'s clean-refusal
//! (cli/verify.rs) and gate-trust's ingestion guard (gate_trust.rs) -- so a
//! third hardcoded copy would only grow the drift risk this module closes.

/// Skills that have a `legion quality-gate check` validator. A skill named
/// here can never earn a "clean" verdict on the ledger via `quality-gate
/// record` -- it must go through `check`, which validates a substantive,
/// per-changed-file articulation before recording (see
/// `crate::simplify_check::validate_articulation` for `legion-simplify`, and
/// `crate::pr_write::validate_pr_body` via
/// `cli::pr::validate_and_record_pr_write_gate` for `legion-pr-write`).
const CHECK_GATED_SKILLS: &[&str] = &["legion-simplify", "legion-pr-write"];

/// True when `skill` has a check validator (see `CHECK_GATED_SKILLS`).
///
/// Matches the bare skill name exactly. The verify gate's key format
/// (`legion-verify:<card_id>`, built by `crate::verify::verify_gate_key`)
/// never appears in `CHECK_GATED_SKILLS`, so a card-keyed verify skill
/// correctly reads as "no check validator" without needing to strip the
/// `:<card_id>` suffix first.
pub fn has_check_validator(skill: &str) -> bool {
    CHECK_GATED_SKILLS.contains(&skill)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn simplify_and_pr_write_are_check_gated() {
        assert!(has_check_validator("legion-simplify"));
        assert!(has_check_validator("legion-pr-write"));
    }

    #[test]
    fn review_and_verify_are_not_check_gated() {
        assert!(!has_check_validator("legion-review"));
        assert!(!has_check_validator("legion-verify"));
    }

    #[test]
    fn card_keyed_verify_skill_is_not_check_gated() {
        // legion-verify:<card_id> must not accidentally match a check-gated
        // entry -- it has no validator and never will.
        assert!(!has_check_validator("legion-verify:card-abc-123"));
    }

    #[test]
    fn unknown_skill_is_not_check_gated() {
        assert!(!has_check_validator("some-future-skill"));
        assert!(!has_check_validator(""));
    }
}
