//! Document-shaped criteria helpers (#882 step 1, #933, #931).
//!
//! Originally this file also owned the `card_criteria` join table -- which
//! spec criteria a card declared it was servicing. #933 moved that concept
//! onto the issue (traced criteria are resolved from the requirement
//! directly), and #931 removed the card-bound surface entirely: the table,
//! `set_card_criteria`, and `card_criteria` are gone. What remains is
//! document-shaped, not card-shaped, and has no card dependency at all.

use super::Database;

/// Read the set of criterion ids present in a document's payload
/// (`verification.criteria[].id`). Returns an empty set for a payload with
/// no such array, a corrupt payload, or a document still on the legacy
/// id-less `verification.acceptance` shape.
///
/// `pub(crate)`: `cli::issue`'s create-time trace validation uses this to
/// refuse a `[criteria: ...]` bracket citing an id a traced requirement does
/// not contain.
pub(crate) fn valid_criterion_ids(payload: &str) -> std::collections::HashSet<String> {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(payload) else {
        return std::collections::HashSet::new();
    };
    let Some(arr) = value
        .get("verification")
        .and_then(|v| v.get("criteria"))
        .and_then(|c| c.as_array())
    else {
        return std::collections::HashSet::new();
    };
    arr.iter()
        .filter_map(|v| v.get("id").and_then(|i| i.as_str()))
        .map(str::to_string)
        .collect()
}

impl Database {
    /// Which of `doc_id`'s criteria have a clean verify verdict recorded
    /// against them (#933): "a requirement can be asked which of its
    /// criteria have been serviced by a clean verdict", making completion
    /// computable rather than asserted. Document-shaped, like
    /// `valid_criterion_ids` above -- never depended on a card, so it needed
    /// no change for #931.
    ///
    /// Scans every non-voided, clean `legion-verify:*` quality-gate row.
    /// `verify::decide_spec_multi` (the issue-trace path) writes its
    /// `SpecAcResult` verdicts into the gate's `details.results[]` array,
    /// each entry carrying `spec_doc_id` + `criterion_id` + `verdict`. A
    /// criterion counts as served when at least one such row cites it
    /// against `doc_id` with verdict `"pass"`.
    /// Trust boundary (#945 review, #780/#882): rows written through
    /// `legion verify` passed `decide_spec_multi`'s referential checks
    /// (document, revision, criterion all resolved at verdict time), but
    /// rows written through the free-form `quality-gate record` path are
    /// NOT validated -- verify-family skills have no check validator by
    /// design, and both paths record `GateProvenance::Asserted`, so this
    /// scan cannot tell them apart. A served mark here is therefore as
    /// trustworthy as the gate rows themselves: self-declared, the
    /// system-wide gap #882 tracks. This surface is read-only display
    /// (`legion document view`); nothing gates on it.
    pub fn document_criteria_served(
        &self,
        doc_id: &str,
    ) -> crate::error::Result<std::collections::HashSet<String>> {
        let mut stmt = self.conn.prepare(
            "SELECT details FROM quality_gates \
             WHERE skill LIKE 'legion-verify:%' AND result = 'clean' AND voided_at IS NULL \
               AND details IS NOT NULL",
        )?;
        let mut rows = stmt.query([])?;
        let mut served: std::collections::HashSet<String> = std::collections::HashSet::new();
        while let Some(row) = rows.next()? {
            let details: String = row.get(0)?;
            let Ok(value) = serde_json::from_str::<serde_json::Value>(&details) else {
                continue;
            };
            let Some(results) = value.get("results").and_then(|r| r.as_array()) else {
                continue;
            };
            for entry in results {
                let spec_doc_id = entry.get("spec_doc_id").and_then(|v| v.as_str());
                let criterion_id = entry.get("criterion_id").and_then(|v| v.as_str());
                let verdict = entry.get("verdict").and_then(|v| v.as_str());
                if spec_doc_id == Some(doc_id)
                    && verdict == Some("pass")
                    && let Some(id) = criterion_id
                {
                    served.insert(id.to_string());
                }
            }
        }
        Ok(served)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::testutil::test_db;

    #[test]
    fn valid_criterion_ids_reads_ids_from_verification_criteria() {
        let payload = serde_json::json!({
            "verification": {
                "criteria": [
                    {"id": "crit-a", "text": "first"},
                    {"id": "crit-b", "text": "second"}
                ]
            }
        })
        .to_string();
        let ids = valid_criterion_ids(&payload);
        assert_eq!(ids.len(), 2);
        assert!(ids.contains("crit-a"));
        assert!(ids.contains("crit-b"));
    }

    #[test]
    fn valid_criterion_ids_empty_for_legacy_acceptance_shape() {
        let payload =
            serde_json::json!({"verification": {"acceptance": ["plain string"]}}).to_string();
        assert!(valid_criterion_ids(&payload).is_empty());
    }

    #[test]
    fn valid_criterion_ids_empty_for_corrupt_payload() {
        assert!(valid_criterion_ids("not json").is_empty());
    }

    // -- #933: `document_criteria_served` ------------------------------------

    fn record_verify_gate(
        db: &Database,
        skill: &str,
        result: crate::verify::GateResult,
        details: &serde_json::Value,
    ) {
        let details_str = details.to_string();
        db.record_quality_gate(&crate::db::quality_gates::QualityGateInput {
            branch: "feat/x",
            commit_hash: "deadbeef",
            skill,
            result,
            findings_count: 0,
            details: Some(&details_str),
            provenance: crate::verify::GateProvenance::Asserted,
            base: None,
        })
        .expect("record gate");
    }

    #[test]
    fn document_criteria_served_empty_with_no_gates() {
        let db = test_db();
        assert!(
            db.document_criteria_served("FR-SERVED-1")
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn document_criteria_served_reports_ids_with_a_clean_pass_verdict() {
        let db = test_db();
        let details = serde_json::json!({
            "skill": "legion-verify",
            "issue": "owner/repo#7",
            "results": [
                {"spec_doc_id": "FR-SERVED-2", "spec_revision": 1, "criterion_id": "crit-a", "verdict": "pass"},
                {"spec_doc_id": "FR-SERVED-2", "spec_revision": 1, "criterion_id": "crit-b", "verdict": "fail"},
                {"spec_doc_id": "FR-OTHER", "spec_revision": 1, "criterion_id": "crit-c", "verdict": "pass"}
            ]
        });
        record_verify_gate(
            &db,
            "legion-verify:issue-owner/repo#7",
            crate::verify::GateResult::Clean,
            &details,
        );

        let served = db.document_criteria_served("FR-SERVED-2").unwrap();
        assert_eq!(served.len(), 1, "got: {served:?}");
        assert!(served.contains("crit-a"));
        assert!(
            !served.contains("crit-b"),
            "a fail verdict must not count as served"
        );
        assert!(
            !served.contains("crit-c"),
            "a pass cited against a DIFFERENT document must not count"
        );
    }

    #[test]
    fn document_criteria_served_ignores_non_clean_gates() {
        let db = test_db();
        let details = serde_json::json!({
            "results": [
                {"spec_doc_id": "FR-SERVED-3", "spec_revision": 1, "criterion_id": "crit-a", "verdict": "pass"}
            ]
        });
        // Same shape, but the GATE result is "issues" -- decide_spec_multi
        // never returns Proceed here, so this row must not count even
        // though an individual result entry says "pass".
        record_verify_gate(
            &db,
            "legion-verify:issue-owner/repo#9",
            crate::verify::GateResult::Issues,
            &details,
        );

        assert!(
            db.document_criteria_served("FR-SERVED-3")
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn document_criteria_served_ignores_voided_gates() {
        let db = test_db();
        let details = serde_json::json!({
            "results": [
                {"spec_doc_id": "FR-SERVED-4", "spec_revision": 1, "criterion_id": "crit-a", "verdict": "pass"}
            ]
        });
        let row = db
            .record_quality_gate(&crate::db::quality_gates::QualityGateInput {
                branch: "feat/x",
                commit_hash: "deadbeef",
                skill: "legion-verify:issue-owner/repo#11",
                result: crate::verify::GateResult::Clean,
                findings_count: 0,
                details: Some(&details.to_string()),
                provenance: crate::verify::GateProvenance::Asserted,
                base: None,
            })
            .expect("record gate");
        db.void_quality_gate(&row.id, "superseded", None)
            .expect("void gate");

        assert!(
            db.document_criteria_served("FR-SERVED-4")
                .unwrap()
                .is_empty()
        );
    }
}
