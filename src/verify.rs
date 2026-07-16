// Verify gate (#520).
//
// The final gate before a card reaches Done. After build, simplify, PR-write,
// and review, verify confirms each acceptance criterion is actually satisfied
// -- with cited evidence -- and emits a per-criterion verdict. For a solo
// engineering team this is the QA the operator does not have: non-skippable,
// gating the kanban ->Done transition.
//
// This module is the decision engine. The verify skill does the judging (reads
// the criteria, the diff, and the test output, and decides per criterion);
// this code makes the judgment binding. Given the criteria and the agent's
// per-criterion verdicts it:
//   - refuses an issue with no checkable criteria (forces SOLID issues),
//   - refuses to proceed unless every criterion has a verdict,
//   - never lets an unprovable criterion pass -- a Pass with no cited evidence
//     or with only vacuous evidence is downgraded to Uncertain ("never assert
//     an unprovable criterion"),
//   - maps the verdict set to one routing decision: any Fail hard-blocks
//     ->Done; any Uncertain (no Fail) routes the card to NeedsInput; all Pass
//     proceeds.

use std::fmt;
use std::str::FromStr;

use crate::error::{LegionError, Result};

/// Result of a quality gate run: either clean (no issues) or issues found.
///
/// Stored as lowercase string in the `quality_gates.result` column so the
/// SQL stays human-readable. Parse and display are symmetric: the closed
/// set of valid values is enforced at the boundary rather than scattered
/// across `== "clean"` comparisons.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GateResult {
    Clean,
    Issues,
}

impl GateResult {
    /// The serialized column value. Display and the SQL write path both
    /// delegate here so the wire form has exactly one source.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Clean => "clean",
            Self::Issues => "issues",
        }
    }
}

impl fmt::Display for GateResult {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for GateResult {
    type Err = LegionError;

    fn from_str(s: &str) -> Result<Self> {
        match s {
            "clean" => Ok(Self::Clean),
            "issues" => Ok(Self::Issues),
            other => Err(LegionError::InvalidGateResult(other.to_string())),
        }
    }
}

/// Provenance of a recorded quality-gate row (#780): whether the verdict was
/// structurally VALIDATED (a coverage-and-substance articulation passed
/// `quality-gate check`) or merely ASSERTED (written via `quality-gate
/// record`, with no validator backing the claim).
///
/// This is the distinction gate-trust and audits need to tell a proven clean
/// gate apart from a self-reported one: the cross-repo ledger the
/// uncertainty engine (#694) treats as calibration ground truth cannot
/// afford to let an unvalidated "clean" claim count the same as a validated
/// one, or a manufactured row silently poisons the very rubber-stamp
/// measurement it exists to produce.
///
/// Stored as lowercase string in `quality_gates.provenance`, mirroring
/// `GateResult`'s Display/FromStr/serde symmetry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GateProvenance {
    /// Recorded via `quality-gate check`: the structural validator (coverage
    /// of every changed file + non-boilerplate substance) passed before this
    /// row was written.
    Validated,
    /// Recorded via `quality-gate record`: no validator ran. Legitimate --
    /// and the only option -- for skills with no check validator
    /// (`legion-review`, a `legion-verify:<card_id>` verdict: asserted by
    /// necessity, since no validator exists for either). For a skill that
    /// DOES have a check validator (`gate_registry::has_check_validator`),
    /// a `clean` result under this provenance is refused at the CLI layer
    /// and, as defense in depth, never ingested by gate-trust even if a row
    /// reaches the ledger some other way (a pre-#780 historical row, or a
    /// future caller that bypasses the CLI).
    Asserted,
}

impl GateProvenance {
    /// The serialized column value. Display and the SQL write path both
    /// delegate here so the wire form has exactly one source.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Validated => "validated",
            Self::Asserted => "asserted",
        }
    }
}

impl fmt::Display for GateProvenance {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for GateProvenance {
    type Err = LegionError;

    fn from_str(s: &str) -> Result<Self> {
        match s {
            "validated" => Ok(Self::Validated),
            "asserted" => Ok(Self::Asserted),
            other => Err(LegionError::InvalidGateProvenance(other.to_string())),
        }
    }
}

/// Build the quality-gate skill key for a verify verdict on `card_id`.
///
/// The key is single-sourced here so that the handler that writes the gate
/// (`cli::verify::handle_verify`) and the handler that reads it
/// (`cli::kanban::handle_done`) cannot drift.
pub fn verify_gate_key(card_id: &str) -> String {
    format!("legion-verify:{card_id}")
}

/// One verdict for one acceptance criterion.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AcVerdict {
    Pass,
    Fail,
    Uncertain,
}

/// The agent's assessment of a single acceptance criterion.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct AcResult {
    pub criterion: String,
    pub verdict: AcVerdict,
    /// A test name, a file:line, or an observable behavior. Required for a
    /// Pass to stand; a Pass with empty or vacuous evidence is treated as
    /// Uncertain.
    pub evidence: String,
}

/// The aggregate routing decision for a card's ->Done transition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VerifyDecision {
    /// Every criterion passed with evidence. Allow ->Done.
    Proceed,
    /// At least one criterion failed. Hard block; the work is not done.
    Block { failed: Vec<String> },
    /// At least one criterion is unprovable (and none failed). Route the card
    /// to NeedsInput so a human adjudicates -- never rubber-stamped to Done.
    NeedsInput { uncertain: Vec<String> },
    /// The issue declared no acceptance criteria. Block: there is nothing to
    /// verify against, which means the issue was not SOLID. Forces criteria
    /// upstream rather than letting unverifiable work reach Done.
    NoCheckableAc,
    /// Fewer verdicts than criteria: some criterion was never assessed. Block
    /// until every criterion has a verdict.
    Incomplete { unaddressed: usize },
    /// The work deviates from the card's frozen acceptance criteria and no
    /// ratified `ReplanRecord` exists for the card (#554, spec-revision
    /// protocol: docs/decisions/2026-05-31-spec-revision-protocol.md). Hard
    /// block -- an unratified deviation is improvisation, not a sanctioned
    /// re-plan. Distinct from `Block` so the message names the missing
    /// ratification rather than a plain unmet criterion.
    ReplanRequired { reason: String },
}

impl VerifyDecision {
    /// True only for `Proceed`: the single state that may reach Done.
    pub fn allows_done(&self) -> bool {
        matches!(self, VerifyDecision::Proceed)
    }
}

/// Returns true when the evidence string is vacuous: it carries no real proof
/// that the criterion was exercised.
///
/// Two conditions count as vacuous:
///
/// 1. **Restatement** -- the evidence is a substring of the criterion
///    (case-insensitive), meaning the agent copied the criterion text rather
///    than citing proof. Example: criterion "returns error on empty input",
///    evidence "returns error on empty input" -- vacuous.
///
/// 2. **No assertion marker** -- the evidence contains none of the tokens that
///    a real test or observable behavior would carry:
///    - a test path token ("::" or "fn " or a source path containing "/")
///    - "assert" (as in an assertion that ran)
///    - a file:line reference (a digit immediately after ":")
///    - observable-behavior language ("returns", "panics", "output", "exits",
///      "prints", "emits", "displays")
///
/// The heuristic is intentionally narrow and conservative: false-negatives
/// (letting a weak-but-real citation through) are acceptable. False-positives
/// (flagging real evidence as vacuous) are not. Where the heuristic cannot
/// decide, the verify SKILL instructs the agent to apply human judgment.
///
/// This function is a pure predicate: it never panics, and handles empty
/// strings and unicode safely.
fn is_vacuous_evidence(criterion: &str, evidence: &str) -> bool {
    let ev = evidence.trim();
    if ev.is_empty() {
        // Also handled by the empty-evidence check in effective(); treat as
        // vacuous so both checks compose cleanly.
        return true;
    }

    let ev_lower: String = ev.to_lowercase();
    let crit_lower: String = criterion.trim().to_lowercase();

    // Condition 1: evidence is a substring of the criterion (restatement).
    if !crit_lower.is_empty() && crit_lower.contains(ev_lower.as_str()) {
        return true;
    }

    // Condition 2: no assertion marker present.
    let has_marker = ev_lower.contains("::")
        || ev_lower.contains("fn ")
        || ev_lower.contains('/')
        || ev_lower.contains("assert")
        || ev_lower.contains("returns")
        || ev_lower.contains("panics")
        || ev_lower.contains("output")
        || ev_lower.contains("exits")
        || ev_lower.contains("prints")
        || ev_lower.contains("emits")
        || ev_lower.contains("displays")
        // file:line: a ":" followed immediately by one or more ASCII digits
        || has_file_line_ref(ev);

    !has_marker
}

/// True when the string contains a colon followed immediately by at least one
/// ASCII digit, which is the typical file:line pattern (e.g. "src/verify.rs:84").
fn has_file_line_ref(s: &str) -> bool {
    let bytes = s.as_bytes();
    for i in 0..bytes.len().saturating_sub(1) {
        if bytes[i] == b':' && bytes[i + 1].is_ascii_digit() {
            return true;
        }
    }
    false
}

/// Decide a card's fate from its acceptance criteria and the agent's verdicts.
///
/// `acceptance` is the criterion list (one per item). `results` is one verdict
/// per criterion. The function is total and side-effect free; the caller
/// records the gate and performs any card transition.
pub fn decide(acceptance: &[String], results: &[AcResult]) -> VerifyDecision {
    if acceptance.is_empty() {
        return VerifyDecision::NoCheckableAc;
    }
    if results.len() < acceptance.len() {
        return VerifyDecision::Incomplete {
            unaddressed: acceptance.len() - results.len(),
        };
    }

    // A Pass with no cited evidence, or with only vacuous evidence (a
    // restatement of the criterion or text with no assertion marker), is not a
    // provable pass -- demote it to Uncertain so it routes to a human instead
    // of rubber-stamping ->Done.
    let effective = |r: &AcResult| -> AcVerdict {
        if r.verdict == AcVerdict::Pass
            && (r.evidence.trim().is_empty() || is_vacuous_evidence(&r.criterion, &r.evidence))
        {
            AcVerdict::Uncertain
        } else {
            r.verdict
        }
    };

    let failed: Vec<String> = results
        .iter()
        .filter(|r| effective(r) == AcVerdict::Fail)
        .map(|r| r.criterion.clone())
        .collect();
    if !failed.is_empty() {
        return VerifyDecision::Block { failed };
    }

    let uncertain: Vec<String> = results
        .iter()
        .filter(|r| effective(r) == AcVerdict::Uncertain)
        .map(|r| r.criterion.clone())
        .collect();
    if !uncertain.is_empty() {
        return VerifyDecision::NeedsInput { uncertain };
    }

    VerifyDecision::Proceed
}

/// The spec-revision deviation gate (#554,
/// docs/decisions/2026-05-31-spec-revision-protocol.md).
///
/// The RFC's audit signal is one bit: was a deviation from the card's frozen
/// acceptance criteria ratified? `deviation` carries the caller's assertion
/// that the work diverges from the frozen AC on its own authority (a reason,
/// not just a flag -- it is what a human re-ratifies against);
/// `ratified_replan_exists` is whether the card has a `ReplanRecord` with
/// `ratified == true`.
///
/// Returns `Some(VerifyDecision::ReplanRequired)` -- a hard block -- only
/// when a deviation is asserted and no ratified re-plan covers it. Returns
/// `None` otherwise, meaning: no deviation was asserted, or one was asserted
/// and a ratified `ReplanRecord` exists, in which case normal `decide()`
/// auditing against the (now current, ratified) acceptance criteria applies
/// unchanged.
///
/// NOTE: this gate is only as good as its input. `deviation` is not yet
/// derived automatically from the per-criterion verdicts the verify skill
/// produces -- today it is surfaced by an explicit `--deviation` flag on
/// `legion verify` (see `cli::verify::handle_verify`) for a human or agent
/// who has judged the frozen AC wrong. Without that flag (or a future
/// automated signal from the skill), this gate does not fire and is a no-op.
pub fn replan_gate(
    deviation: Option<&str>,
    ratified_replan_exists: bool,
) -> Option<VerifyDecision> {
    let reason = deviation?;
    if ratified_replan_exists {
        return None;
    }
    Some(VerifyDecision::ReplanRequired {
        reason: format!(
            "unratified deviation from the frozen acceptance criteria: {reason} \
             -- no ratified ReplanRecord exists for this card. Either the work matches \
             the frozen AC, or the re-plan must be ratified first \
             (`legion kanban replan-request` then `legion kanban replan-record`) before \
             verify can audit against revised criteria."
        ),
    })
}

/// Split a card's stored `acceptance` field (newline-joined criteria, as
/// `db::insert_card` stores them) into individual criteria, dropping blank
/// lines. Returns an empty vec for `None` or an all-blank field, which
/// `decide` reports as `NoCheckableAc`.
pub fn acceptance_items(stored: Option<&str>) -> Vec<String> {
    stored
        .unwrap_or("")
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_owned)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn res(criterion: &str, verdict: AcVerdict, evidence: &str) -> AcResult {
        AcResult {
            criterion: criterion.to_owned(),
            verdict,
            evidence: evidence.to_owned(),
        }
    }

    #[test]
    fn all_pass_proceeds() {
        let ac = vec!["A".to_owned(), "B".to_owned()];
        let results = vec![
            res("A", AcVerdict::Pass, "tests::a"),
            res("B", AcVerdict::Pass, "src/x.rs:10"),
        ];
        assert_eq!(decide(&ac, &results), VerifyDecision::Proceed);
        assert!(decide(&ac, &results).allows_done());
    }

    #[test]
    fn any_fail_blocks() {
        let ac = vec!["A".to_owned(), "B".to_owned()];
        let results = vec![
            res("A", AcVerdict::Pass, "tests::a"),
            res("B", AcVerdict::Fail, "no handler for the empty case"),
        ];
        let d = decide(&ac, &results);
        assert_eq!(
            d,
            VerifyDecision::Block {
                failed: vec!["B".to_owned()]
            }
        );
        assert!(!d.allows_done());
    }

    #[test]
    fn uncertain_routes_to_needs_input() {
        let ac = vec!["A".to_owned(), "B".to_owned()];
        let results = vec![
            res("A", AcVerdict::Pass, "tests::a"),
            res(
                "B",
                AcVerdict::Uncertain,
                "cannot mechanically check perf claim",
            ),
        ];
        assert_eq!(
            decide(&ac, &results),
            VerifyDecision::NeedsInput {
                uncertain: vec!["B".to_owned()]
            }
        );
    }

    #[test]
    fn fail_outranks_uncertain() {
        let ac = vec!["A".to_owned(), "B".to_owned()];
        let results = vec![
            res("A", AcVerdict::Uncertain, "unprovable"),
            res("B", AcVerdict::Fail, "broken"),
        ];
        // A Fail anywhere is a hard block regardless of uncertain criteria.
        assert!(matches!(
            decide(&ac, &results),
            VerifyDecision::Block { .. }
        ));
    }

    #[test]
    fn pass_without_evidence_is_uncertain_never_pass() {
        let ac = vec!["A".to_owned()];
        // Claims Pass but cites nothing -- not a provable pass.
        let results = vec![res("A", AcVerdict::Pass, "   ")];
        assert_eq!(
            decide(&ac, &results),
            VerifyDecision::NeedsInput {
                uncertain: vec!["A".to_owned()]
            }
        );
    }

    #[test]
    fn empty_acceptance_is_blocked() {
        assert_eq!(decide(&[], &[]), VerifyDecision::NoCheckableAc);
    }

    #[test]
    fn fewer_verdicts_than_criteria_is_incomplete() {
        let ac = vec!["A".to_owned(), "B".to_owned(), "C".to_owned()];
        let results = vec![res("A", AcVerdict::Pass, "tests::a")];
        assert_eq!(
            decide(&ac, &results),
            VerifyDecision::Incomplete { unaddressed: 2 }
        );
    }

    // --- spec-revision deviation gate (#554) ---

    #[test]
    fn replan_gate_noop_without_deviation() {
        assert_eq!(replan_gate(None, false), None);
        assert_eq!(replan_gate(None, true), None);
    }

    #[test]
    fn replan_gate_blocks_unratified_deviation() {
        let decision = replan_gate(Some("step 2 revealed step 1's AC was unachievable"), false);
        match decision {
            Some(VerifyDecision::ReplanRequired { reason }) => {
                assert!(reason.contains("unratified deviation"));
                assert!(reason.contains("step 2 revealed"));
            }
            other => panic!("expected ReplanRequired, got {other:?}"),
        }
    }

    #[test]
    fn replan_gate_allows_ratified_deviation() {
        // A ratified ReplanRecord covers the deviation -- normal decide()
        // auditing against the revised AC applies, so the gate is a no-op.
        assert_eq!(
            replan_gate(Some("AC was revised after re-plan"), true),
            None
        );
    }

    #[test]
    fn replan_required_never_allows_done() {
        let decision = VerifyDecision::ReplanRequired {
            reason: "x".to_owned(),
        };
        assert!(!decision.allows_done());
    }

    #[test]
    fn acceptance_items_splits_and_trims() {
        assert_eq!(
            acceptance_items(Some("First\n  Second  \n\nThird\n")),
            vec!["First".to_owned(), "Second".to_owned(), "Third".to_owned()]
        );
        assert!(acceptance_items(None).is_empty());
        assert!(acceptance_items(Some("\n  \n")).is_empty());
    }

    // --- vacuous evidence tests (added by #549) ---

    #[test]
    fn vacuous_evidence_restatement_is_uncertain() {
        // Evidence that exactly restates the criterion is not proof.
        let ac = vec!["returns error on empty input".to_owned()];
        let results = vec![res(
            "returns error on empty input",
            AcVerdict::Pass,
            "returns error on empty input",
        )];
        assert_eq!(
            decide(&ac, &results),
            VerifyDecision::NeedsInput {
                uncertain: vec!["returns error on empty input".to_owned()]
            }
        );
    }

    #[test]
    fn vacuous_evidence_echo_is_uncertain() {
        // Evidence that describes only what the code does (no test path, no
        // assertion, no observable behavior) is vacuous.
        let ac = vec!["handles the empty case".to_owned()];
        let results = vec![res(
            "handles the empty case",
            AcVerdict::Pass,
            "added match arm for empty case",
        )];
        assert_eq!(
            decide(&ac, &results),
            VerifyDecision::NeedsInput {
                uncertain: vec!["handles the empty case".to_owned()]
            }
        );
    }

    #[test]
    fn real_evidence_passes() {
        // A test path with "::" is real evidence.
        let ac = vec!["returns error on empty input".to_owned()];
        let results = vec![res(
            "returns error on empty input",
            AcVerdict::Pass,
            "tests::empty_input_returns_error",
        )];
        assert_eq!(decide(&ac, &results), VerifyDecision::Proceed);
    }

    #[test]
    fn file_line_evidence_passes() {
        // A file:line reference is real evidence.
        let ac = vec!["is_vacuous_evidence demotes restatements".to_owned()];
        let results = vec![res(
            "is_vacuous_evidence demotes restatements",
            AcVerdict::Pass,
            "src/verify.rs:84",
        )];
        assert_eq!(decide(&ac, &results), VerifyDecision::Proceed);
    }

    #[test]
    fn observable_behavior_passes() {
        // Observable-behavior language is real evidence.
        let ac = vec!["exits non-zero on no AC".to_owned()];
        let results = vec![res(
            "exits non-zero on no AC",
            AcVerdict::Pass,
            "running legion verify --card X on a card with no AC exits 1 and prints NoCheckableAc",
        )];
        assert_eq!(decide(&ac, &results), VerifyDecision::Proceed);
    }

    // --- is_vacuous_evidence unit tests ---

    #[test]
    fn is_vacuous_empty_evidence() {
        assert!(is_vacuous_evidence("some criterion", ""));
        assert!(is_vacuous_evidence("some criterion", "   "));
    }

    #[test]
    fn is_vacuous_restatement_case_insensitive() {
        assert!(is_vacuous_evidence(
            "Returns Error On Empty Input",
            "returns error on empty input"
        ));
    }

    #[test]
    fn is_vacuous_partial_restatement_substring() {
        // Evidence is a substring of the criterion -- still vacuous.
        assert!(is_vacuous_evidence(
            "returns error on empty input and logs",
            "returns error on empty input"
        ));
    }

    #[test]
    fn is_not_vacuous_test_path() {
        assert!(!is_vacuous_evidence(
            "some criterion",
            "tests::my_function_works"
        ));
    }

    #[test]
    fn is_not_vacuous_file_line() {
        assert!(!is_vacuous_evidence("some criterion", "src/lib.rs:42"));
    }

    #[test]
    fn is_not_vacuous_assert_language() {
        assert!(!is_vacuous_evidence(
            "some criterion",
            "assert_eq! confirms the value is zero"
        ));
    }

    #[test]
    fn is_not_vacuous_observable_behavior() {
        assert!(!is_vacuous_evidence(
            "some criterion",
            "the command exits with code 1"
        ));
    }

    #[test]
    fn is_vacuous_unicode_safe() {
        // Unicode text with no assertion markers is vacuous; must not panic.
        assert!(is_vacuous_evidence("critere", "\u{00e9}l\u{00e8}ve"));
    }

    // --- GateResult tests ---

    #[test]
    fn gate_result_display_roundtrip() {
        // Every variant serializes to lowercase and parses back exactly.
        for r in [GateResult::Clean, GateResult::Issues] {
            let s = r.to_string();
            let parsed = s.parse::<GateResult>().expect("parse should succeed");
            assert_eq!(r, parsed, "display/parse roundtrip failed for {r}");
        }
    }

    #[test]
    fn gate_result_display_values() {
        assert_eq!(GateResult::Clean.to_string(), "clean");
        assert_eq!(GateResult::Issues.to_string(), "issues");
    }

    #[test]
    fn gate_result_parse_invalid_returns_err() {
        // Typos and case variants are rejected at the parse boundary.
        assert!("unknown".parse::<GateResult>().is_err());
        assert!("Clean".parse::<GateResult>().is_err());
        assert!("CLEAN".parse::<GateResult>().is_err());
        assert!("".parse::<GateResult>().is_err());
    }

    // --- GateProvenance tests (#780) ---

    #[test]
    fn gate_provenance_display_roundtrip() {
        for p in [GateProvenance::Validated, GateProvenance::Asserted] {
            let s = p.to_string();
            let parsed = s.parse::<GateProvenance>().expect("parse should succeed");
            assert_eq!(p, parsed, "display/parse roundtrip failed for {p}");
        }
    }

    #[test]
    fn gate_provenance_display_values() {
        assert_eq!(GateProvenance::Validated.to_string(), "validated");
        assert_eq!(GateProvenance::Asserted.to_string(), "asserted");
    }

    #[test]
    fn gate_provenance_parse_invalid_returns_err() {
        assert!("unknown".parse::<GateProvenance>().is_err());
        assert!("Validated".parse::<GateProvenance>().is_err());
        assert!("".parse::<GateProvenance>().is_err());
    }

    // --- verify_gate_key tests ---

    #[test]
    fn verify_gate_key_format() {
        // The key is the single-source for the card-keyed gate used by
        // handle_verify (write site) and handle_done (read site).
        let key = verify_gate_key("card-abc-123");
        assert_eq!(key, "legion-verify:card-abc-123");
    }
}
