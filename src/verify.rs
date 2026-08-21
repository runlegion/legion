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

/// Build the quality-gate skill key for a verify verdict on a work-source
/// issue (#913). Namespaced `issue-<n>` so it cannot collide with the old
/// card-keyed format (`legion-verify:<card-uuid>`, retired with the card
/// surface, #931).
///
/// Scoped by `source_repo` because issue numbers are only unique within a
/// work-source repo: two legion-watched repos both pointing at their own
/// GitHub project would otherwise share a gate row for their respective
/// issue #12.
pub fn verify_gate_key_for_issue(source_repo: &str, issue: u64) -> String {
    format!("legion-verify:issue-{source_repo}#{issue}")
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

/// One criterion with a stable identity (#882 step 1), as carried in a
/// document's `verification.criteria` array (`[{id, text}]`). `id` is a
/// UUIDv7 assigned once at first write and preserved across revisions by
/// `Database::revise_document` -- it is what a spec-bound verdict
/// (`SpecAcResult`) cites, instead of matching against free prose.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpecCriterion {
    pub id: String,
    pub text: String,
}

/// The kind of resolvable artifact backing a spec-bound verdict (#882 step
/// 3): a test that ran, a diff hunk/path, or a command invocation. Stored
/// lowercase in JSON, mirroring `AcVerdict`/`GateResult`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ArtifactKind {
    Test,
    Diff,
    Command,
}

/// One piece of resolvable evidence for a spec-bound verdict: something a
/// reader (or a future validator) can go look up, not prose. `reference`
/// serializes as `ref` on the wire -- `ref` is a Rust keyword, so the field
/// is named `reference` and renamed at the serde boundary.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct VerdictArtifact {
    pub kind: ArtifactKind,
    #[serde(rename = "ref")]
    pub reference: String,
    pub outcome: String,
}

/// One verdict against a spec-bound criterion (#882 step 3). Replaces free
/// -text evidence (`AcResult`) with a resolvable reference -- which spec
/// document, which revision, which criterion id -- plus structured
/// artifacts. Used only for cards bound to a document whose payload carries
/// `verification.criteria` (id-carrying); a card on the legacy path
/// (`tasks.acceptance`, or a document with only `verification.acceptance`
/// strings) keeps submitting `AcResult`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SpecAcResult {
    pub spec_doc_id: String,
    pub spec_revision: i64,
    pub criterion_id: String,
    pub verdict: AcVerdict,
    #[serde(default)]
    pub artifacts: Vec<VerdictArtifact>,
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
    /// A `SpecAcResult` cited a spec reference that does not resolve (#882
    /// step 3): the wrong spec document, a stale revision, or a criterion id
    /// absent from the document's current criteria set. Hard block -- a
    /// verdict that does not name real spec state proves nothing, so it
    /// never reaches the Pass/Fail/Uncertain accounting below it. `details`
    /// carries one human-readable line per bad citation naming exactly
    /// which condition failed (wrong document / stale revision / unknown
    /// id), so the caller never has to guess which check tripped.
    InvalidCriterionReference { details: Vec<String> },
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

/// One requirement an issue's trace (#933) cites, scoped to the specific
/// document state a `verify --issue` run resolved: which document, which
/// revision it is currently at, and the criterion subset the trace bullet
/// named (or the whole requirement, when the bullet carried no
/// `[criteria: ...]` bracket). Callers build one of these per traced
/// requirement bullet before calling `decide_spec_multi`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TracedRequirement {
    pub doc_id: String,
    pub revision: i64,
    pub criteria: Vec<SpecCriterion>,
}

/// Decide a work-source issue's fate against the requirement(s) its trace
/// (#933) cites. A trace may name more than one requirement -- the contract
/// permits "an issue may service two requirements" -- so there is no single
/// `(expected_doc_id, expected_revision)` pair the whole verdict set is
/// checked against: each criterion is scoped to the specific document (and
/// revision) its OWN traced requirement named.
///
/// Coverage is computed by **id set**, not by count: two verdicts citing the
/// same criterion id satisfy that one id twice and leave every other
/// criterion uncovered. Evidence and pass/fail/uncertain accounting mirror
/// `decide()`'s vacuous-evidence rule for the legacy prose format (#549,
/// HIGH-1 review fix): a reference must clear `is_vacuous_evidence` against
/// the criterion text, not merely be non-empty.
///
/// Criteria are keyed by `(doc_id, criterion_id)` throughout -- never by
/// criterion id alone. Criterion ids are only unique WITHIN one document
/// (`insert_document` preserves caller-supplied ids verbatim and checks
/// duplicates only against the document's own payload), so two traced
/// requirements can legitimately share an id string like `crit-1`. A
/// single-id key would let the first requirement's criterion claim the
/// slot: a correct citation against the second document would be refused
/// as mis-cited, and -- worse, silently -- the second document's same-id
/// criterion would vanish from coverage, letting verify Proceed without it
/// ever being judged.
pub fn decide_spec_multi(
    requirements: &[TracedRequirement],
    results: &[SpecAcResult],
) -> VerifyDecision {
    let mut owner: std::collections::HashMap<(&str, &str), (i64, &str)> =
        std::collections::HashMap::new();
    for req in requirements {
        for c in &req.criteria {
            owner
                .entry((req.doc_id.as_str(), c.id.as_str()))
                .or_insert((req.revision, c.text.as_str()));
        }
    }
    if owner.is_empty() {
        return VerifyDecision::NoCheckableAc;
    }

    // Human-readable label for a result: the criterion text where the
    // citation resolves, the raw id otherwise.
    let text_for = |r: &SpecAcResult| -> String {
        owner
            .get(&(r.spec_doc_id.as_str(), r.criterion_id.as_str()))
            .map(|(_, text)| (*text).to_string())
            .unwrap_or_else(|| r.criterion_id.clone())
    };

    // Every citation must resolve to the exact (document, revision) its own
    // traced requirement named -- checked before coverage, same ordering
    // rationale as `decide_spec`: a bogus citation is always reported as
    // exactly that, never mistaken for a missing verdict.
    let invalid: Vec<String> = results
        .iter()
        .filter_map(|r| {
            match owner.get(&(r.spec_doc_id.as_str(), r.criterion_id.as_str())) {
                None => {
                    // Distinguish "right criterion, wrong document" from
                    // "no traced requirement contains this id at all".
                    let id_elsewhere = owner
                        .keys()
                        .find(|(_, cid)| *cid == r.criterion_id.as_str());
                    match id_elsewhere {
                        Some((doc_id, _)) => Some(format!(
                            "criterion '{}': cites spec document '{}', but this issue's \
                             trace scopes that id to '{doc_id}'",
                            r.criterion_id, r.spec_doc_id
                        )),
                        None => Some(format!(
                            "criterion id '{}' does not exist in any requirement this \
                             issue traces to",
                            r.criterion_id
                        )),
                    }
                }
                Some((revision, _)) => {
                    if r.spec_revision != *revision {
                        Some(format!(
                            "criterion '{}': cites spec revision {}, expected {revision} \
                             (stale reference -- the spec was revised since this verdict \
                             was formed)",
                            r.criterion_id, r.spec_revision
                        ))
                    } else {
                        None
                    }
                }
            }
        })
        .collect();
    if !invalid.is_empty() {
        return VerifyDecision::InvalidCriterionReference { details: invalid };
    }

    // Past this point every result names a real (document, revision,
    // criterion) triple. Coverage by (document, criterion) pair across ALL
    // traced requirements -- a same-id criterion in a second document is
    // its own entry and must be cited separately.
    let valid_keys: std::collections::HashSet<(&str, &str)> = owner.keys().copied().collect();
    let cited: std::collections::HashSet<(&str, &str)> = results
        .iter()
        .map(|r| (r.spec_doc_id.as_str(), r.criterion_id.as_str()))
        .collect();
    let unaddressed = valid_keys.difference(&cited).count();
    if unaddressed > 0 {
        return VerifyDecision::Incomplete { unaddressed };
    }

    let effective = |r: &SpecAcResult| -> AcVerdict {
        let criterion_text = text_for(r);
        let has_evidence = r.artifacts.iter().any(|a| {
            !a.outcome.trim().is_empty() && !is_vacuous_evidence(&criterion_text, &a.reference)
        });
        if r.verdict == AcVerdict::Pass && !has_evidence {
            AcVerdict::Uncertain
        } else {
            r.verdict
        }
    };

    let failed: Vec<String> = results
        .iter()
        .filter(|r| effective(r) == AcVerdict::Fail)
        .map(&text_for)
        .collect();
    if !failed.is_empty() {
        return VerifyDecision::Block { failed };
    }

    let uncertain: Vec<String> = results
        .iter()
        .filter(|r| effective(r) == AcVerdict::Uncertain)
        .map(&text_for)
        .collect();
    if !uncertain.is_empty() {
        return VerifyDecision::NeedsInput { uncertain };
    }

    VerifyDecision::Proceed
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

    // --- verify_gate_key_for_issue tests ---

    #[test]
    fn verify_gate_key_for_issue_is_scoped_by_source_repo() {
        // Issue numbers are only unique within a work-source repo, so two
        // repos' issue #12 must not collide on one gate row.
        assert_eq!(
            verify_gate_key_for_issue("runlegion/legion", 12),
            "legion-verify:issue-runlegion/legion#12"
        );
        assert_ne!(
            verify_gate_key_for_issue("runlegion/legion", 12),
            verify_gate_key_for_issue("rafters-studio/smugglr", 12),
        );
    }

    // decide_spec (the single-document #882 step 3 predecessor to
    // decide_spec_multi below) was removed with the card surface (#931):
    // its only caller was the card-bound verify path. decide_spec_multi is
    // a strict generalization (N traced requirements, N=1 being the
    // single-document case decide_spec used to cover) and carries its own
    // equivalent coverage below, reusing the same `crit`/`artifact`/
    // `spec_res` builders.

    fn crit(id: &str, text: &str) -> SpecCriterion {
        SpecCriterion {
            id: id.to_owned(),
            text: text.to_owned(),
        }
    }

    fn artifact(reference: &str, outcome: &str) -> VerdictArtifact {
        VerdictArtifact {
            kind: ArtifactKind::Test,
            reference: reference.to_owned(),
            outcome: outcome.to_owned(),
        }
    }

    fn spec_res(
        doc_id: &str,
        revision: i64,
        criterion_id: &str,
        verdict: AcVerdict,
        artifacts: Vec<VerdictArtifact>,
    ) -> SpecAcResult {
        SpecAcResult {
            spec_doc_id: doc_id.to_owned(),
            spec_revision: revision,
            criterion_id: criterion_id.to_owned(),
            verdict,
            artifacts,
        }
    }

    // --- decide_spec_multi tests (#933: multi-requirement issue traces) ---

    fn traced(doc_id: &str, revision: i64, criteria: Vec<SpecCriterion>) -> TracedRequirement {
        TracedRequirement {
            doc_id: doc_id.to_owned(),
            revision,
            criteria,
        }
    }

    #[test]
    fn decide_spec_multi_empty_requirements_is_blocked() {
        assert_eq!(decide_spec_multi(&[], &[]), VerifyDecision::NoCheckableAc);
    }

    /// #945 review HIGH, execution-verified: criterion ids are only unique
    /// within one document, so two traced requirements sharing an id string
    /// (human-chosen ids like "crit-1") must remain distinct entries. A
    /// correct citation against the SECOND document's same-named criterion
    /// must proceed, not be refused as mis-cited to the first.
    #[test]
    fn decide_spec_multi_same_criterion_id_in_two_requirements_both_judged() {
        let requirements = vec![
            traced("doc-1", 1, vec![crit("crit-1", "first doc's crit")]),
            traced("doc-2", 3, vec![crit("crit-1", "second doc's crit")]),
        ];
        let results = vec![
            spec_res(
                "doc-1",
                1,
                "crit-1",
                AcVerdict::Pass,
                vec![artifact("tests::a", "passed")],
            ),
            spec_res(
                "doc-2",
                3,
                "crit-1",
                AcVerdict::Pass,
                vec![artifact("tests::b", "passed")],
            ),
        ];
        assert_eq!(
            decide_spec_multi(&requirements, &results),
            VerifyDecision::Proceed
        );
    }

    /// The silent half of the same collision: citing only the first
    /// document's criterion must leave the second document's same-id
    /// criterion counted as unaddressed -- it must not vanish from coverage.
    #[test]
    fn decide_spec_multi_same_id_second_document_still_requires_its_own_citation() {
        let requirements = vec![
            traced("doc-1", 1, vec![crit("crit-1", "first doc's crit")]),
            traced("doc-2", 3, vec![crit("crit-1", "second doc's crit")]),
        ];
        let results = vec![spec_res(
            "doc-1",
            1,
            "crit-1",
            AcVerdict::Pass,
            vec![artifact("tests::a", "passed")],
        )];
        assert_eq!(
            decide_spec_multi(&requirements, &results),
            VerifyDecision::Incomplete { unaddressed: 1 }
        );
    }

    /// Ordering pin (#945 review): a call carrying BOTH an invalid citation
    /// and an unaddressed criterion must report the invalid citation --
    /// bogus references are never mistaken for missing verdicts.
    #[test]
    fn decide_spec_multi_invalid_citation_reported_before_coverage_gap() {
        let requirements = vec![traced(
            "doc-1",
            1,
            vec![crit("crit-1", "first"), crit("crit-2", "second")],
        )];
        let results = vec![spec_res(
            "doc-1",
            9,
            "crit-1",
            AcVerdict::Pass,
            vec![artifact("tests::a", "passed")],
        )];
        match decide_spec_multi(&requirements, &results) {
            VerifyDecision::InvalidCriterionReference { details } => {
                assert!(
                    details[0].contains("revision"),
                    "expected the stale-revision refusal, got: {details:?}"
                );
            }
            other => panic!("expected InvalidCriterionReference, got {other:?}"),
        }
    }

    /// The headline proof for the multi-requirement case: an issue tracing
    /// to two different requirements, each with its own document id and
    /// revision, must resolve verdicts against the SPECIFIC requirement each
    /// criterion belongs to -- not one shared expected document.
    #[test]
    fn decide_spec_multi_two_requirements_all_pass_proceeds() {
        let requirements = vec![
            traced("doc-1", 1, vec![crit("crit-1", "first thing")]),
            traced("doc-2", 5, vec![crit("crit-2", "second thing")]),
        ];
        let results = vec![
            spec_res(
                "doc-1",
                1,
                "crit-1",
                AcVerdict::Pass,
                vec![artifact("tests::a", "passed")],
            ),
            spec_res(
                "doc-2",
                5,
                "crit-2",
                AcVerdict::Pass,
                vec![artifact("tests::b", "passed")],
            ),
        ];
        assert_eq!(
            decide_spec_multi(&requirements, &results),
            VerifyDecision::Proceed
        );
    }

    /// A verdict citing the RIGHT criterion id but the WRONG requirement's
    /// document (e.g. crit-1 belongs to doc-1, cited against doc-2) must be
    /// rejected -- each criterion is pinned to its own traced requirement.
    #[test]
    fn decide_spec_multi_rejects_criterion_cited_against_wrong_requirement() {
        let requirements = vec![
            traced("doc-1", 1, vec![crit("crit-1", "first thing")]),
            traced("doc-2", 1, vec![crit("crit-2", "second thing")]),
        ];
        let results = vec![
            spec_res(
                "doc-2", // wrong document for crit-1, which belongs to doc-1
                1,
                "crit-1",
                AcVerdict::Pass,
                vec![artifact("tests::a", "passed")],
            ),
            spec_res(
                "doc-2",
                1,
                "crit-2",
                AcVerdict::Pass,
                vec![artifact("tests::b", "passed")],
            ),
        ];
        match decide_spec_multi(&requirements, &results) {
            VerifyDecision::InvalidCriterionReference { details } => {
                assert!(
                    details[0].contains("crit-1")
                        && details[0].contains("scopes that id to 'doc-1'"),
                    "expected a wrong-document message naming doc-1, got: {}",
                    details[0]
                );
            }
            other => panic!("expected InvalidCriterionReference, got {other:?}"),
        }
    }

    /// A stale revision on one of several traced requirements is rejected
    /// distinctly, scoped to that requirement's own expected revision.
    #[test]
    fn decide_spec_multi_rejects_stale_revision_on_one_requirement() {
        let requirements = vec![
            traced("doc-1", 2, vec![crit("crit-1", "first thing")]),
            traced("doc-2", 1, vec![crit("crit-2", "second thing")]),
        ];
        let results = vec![
            spec_res(
                "doc-1",
                1, // stale -- doc-1 is now at revision 2
                "crit-1",
                AcVerdict::Pass,
                vec![artifact("tests::a", "passed")],
            ),
            spec_res(
                "doc-2",
                1,
                "crit-2",
                AcVerdict::Pass,
                vec![artifact("tests::b", "passed")],
            ),
        ];
        match decide_spec_multi(&requirements, &results) {
            VerifyDecision::InvalidCriterionReference { details } => {
                assert!(
                    details[0].contains("stale reference"),
                    "expected a stale-revision message, got: {}",
                    details[0]
                );
            }
            other => panic!("expected InvalidCriterionReference, got {other:?}"),
        }
    }

    /// Coverage spans every traced requirement: a criterion from the second
    /// requirement left unaddressed must block, even though the first
    /// requirement's criterion was fully covered.
    #[test]
    fn decide_spec_multi_coverage_spans_all_requirements() {
        let requirements = vec![
            traced("doc-1", 1, vec![crit("crit-1", "first thing")]),
            traced("doc-2", 1, vec![crit("crit-2", "second thing")]),
        ];
        let results = vec![spec_res(
            "doc-1",
            1,
            "crit-1",
            AcVerdict::Pass,
            vec![artifact("tests::a", "passed")],
        )];
        assert_eq!(
            decide_spec_multi(&requirements, &results),
            VerifyDecision::Incomplete { unaddressed: 1 }
        );
    }

    #[test]
    fn decide_spec_multi_fail_on_one_requirement_blocks() {
        let requirements = vec![
            traced("doc-1", 1, vec![crit("crit-1", "first thing")]),
            traced("doc-2", 1, vec![crit("crit-2", "second thing")]),
        ];
        let results = vec![
            spec_res(
                "doc-1",
                1,
                "crit-1",
                AcVerdict::Pass,
                vec![artifact("tests::a", "passed")],
            ),
            spec_res(
                "doc-2",
                1,
                "crit-2",
                AcVerdict::Fail,
                vec![artifact("tests::b", "failed: no handler")],
            ),
        ];
        assert_eq!(
            decide_spec_multi(&requirements, &results),
            VerifyDecision::Block {
                failed: vec!["second thing".to_owned()]
            }
        );
    }

    /// A Pass with no artifact carrying both a reference and an outcome is
    /// demoted to Uncertain, mirroring `decide()`'s vacuous-evidence rule
    /// (parity check for `decide_spec_multi`'s own `effective` closure,
    /// which the deleted single-document `decide_spec` used to cover).
    #[test]
    fn decide_spec_multi_pass_with_no_artifacts_is_uncertain() {
        let requirements = vec![traced("doc-1", 1, vec![crit("crit-1", "does the thing")])];
        let results = vec![spec_res("doc-1", 1, "crit-1", AcVerdict::Pass, vec![])];
        assert_eq!(
            decide_spec_multi(&requirements, &results),
            VerifyDecision::NeedsInput {
                uncertain: vec!["does the thing".to_owned()]
            }
        );
    }

    #[test]
    fn decide_spec_multi_all_pass_with_evidence_proceeds() {
        let requirements = vec![traced(
            "doc-1",
            3,
            vec![crit("crit-1", "first"), crit("crit-2", "second")],
        )];
        let results = vec![
            spec_res(
                "doc-1",
                3,
                "crit-1",
                AcVerdict::Pass,
                vec![artifact("tests::a", "passed")],
            ),
            spec_res(
                "doc-1",
                3,
                "crit-2",
                AcVerdict::Pass,
                vec![artifact("src/x.rs:10", "returns Ok")],
            ),
        ];
        assert_eq!(
            decide_spec_multi(&requirements, &results),
            VerifyDecision::Proceed
        );
    }
}
