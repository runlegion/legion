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
//     is downgraded to Uncertain ("never assert an unprovable criterion"),
//   - maps the verdict set to one routing decision: any Fail hard-blocks
//     ->Done; any Uncertain (no Fail) routes the card to NeedsInput; all Pass
//     proceeds.

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
    /// Pass to stand; a Pass with empty evidence is treated as Uncertain.
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
}

impl VerifyDecision {
    /// True only for `Proceed`: the single state that may reach Done.
    pub fn allows_done(&self) -> bool {
        matches!(self, VerifyDecision::Proceed)
    }
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

    // A Pass with no cited evidence is not a provable pass -- demote it to
    // Uncertain so it routes to a human instead of rubber-stamping ->Done.
    let effective = |r: &AcResult| -> AcVerdict {
        if r.verdict == AcVerdict::Pass && r.evidence.trim().is_empty() {
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

    #[test]
    fn acceptance_items_splits_and_trims() {
        assert_eq!(
            acceptance_items(Some("First\n  Second  \n\nThird\n")),
            vec!["First".to_owned(), "Second".to_owned(), "Third".to_owned()]
        );
        assert!(acceptance_items(None).is_empty());
        assert!(acceptance_items(Some("\n  \n")).is_empty());
    }
}
