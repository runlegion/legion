// PR-write forcing function (#519).
//
// Articulation is verification. Before a PR can be opened, the agent must
// write -- in prose -- how each acceptance criterion is satisfied by the
// change, cite evidence (a test, a file:line, an observable behavior), and
// state what was deliberately NOT done. Composing that mapping forces the
// agent to re-read its own diff as a reader and catch what it talked past
// while coding. The written body is also the explicit claim set the verify
// gate (#520) later audits.
//
// This module is the objective half of the gate: a structural validator that
// refuses an empty or boilerplate mapping. It does not judge prose quality --
// it enforces that prose of substance exists, one entry per criterion, each
// carrying evidence, plus a non-empty not-done section. The skill
// (plugin/skills/legion-pr-write) tells the agent how to compose a body that
// passes; this code makes "I'll just paste the ACs back" fail.

use crate::card_parse::parse_issue_body;

/// Minimum words of prose per acceptance-criterion mapping entry, after the
/// heading and the evidence line are stripped. A genuine "this change
/// satisfies it because..." explanation clears this easily; a verbatim
/// restatement of a short criterion does not.
pub(crate) const MIN_MAPPING_WORDS: usize = 12;

/// Finding emitted when the body has no "Not done" section. Shared by the
/// early-return (no mapping) path and the main flow so the two cannot drift.
const NOT_DONE_MISSING: &str =
    "Missing the 'Not done' section. State what was deliberately left out of scope.";

/// Result of validating a PR body against an issue's acceptance criteria.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrWriteReport {
    /// True when the body satisfies every structural requirement.
    pub ok: bool,
    /// Human-readable gaps, one per failed requirement. Empty when `ok`.
    pub findings: Vec<String>,
    /// Number of acceptance-criterion mapping entries found in the body.
    pub mapping_entries: usize,
}

/// Validate a drafted PR `body` against the issue's `acceptance` criteria.
///
/// `acceptance` is the criterion list parsed from the issue (see
/// `card_parse::parse_issue_body`). An empty list means the issue declared no
/// machine-readable criteria; the validator still requires a mapping section
/// with at least one substantive entry plus a not-done section, so the
/// forcing function is never a no-op.
pub fn validate_pr_body(acceptance: &[String], body: &str) -> PrWriteReport {
    let mut findings: Vec<String> = Vec::new();

    let parsed = parse_issue_body(body);

    // Locate the two required sections by heading heuristic. parse_issue_body
    // routes "acceptance criteria" / "acceptance" exactly to its own field, so
    // the mapping heading is deliberately distinct ("... mapping") and lands
    // in the generic `sections` bucket.
    let mapping = parsed
        .sections
        .iter()
        .find(|(h, _)| {
            let h = h.to_lowercase();
            h.contains("acceptance") && h.contains("mapping")
        })
        .map(|(_, c)| c.as_str());

    let not_done = parsed
        .sections
        .iter()
        .find(|(h, _)| {
            let h = h.to_lowercase();
            h.contains("not done") || h.contains("out of scope") || h.contains("deliberately")
        })
        .map(|(_, c)| c.as_str());

    let Some(mapping) = mapping else {
        findings.push(
            "Missing the 'Acceptance criteria mapping' section. Map each criterion to the \
             change that satisfies it, in prose, with evidence."
                .to_owned(),
        );
        // No mapping means nothing else to check meaningfully; still report
        // the not-done gap so the agent fixes both at once.
        if not_done.is_none() {
            findings.push(NOT_DONE_MISSING.to_owned());
        }
        return PrWriteReport {
            ok: false,
            findings,
            mapping_entries: 0,
        };
    };

    let entries = split_entries(mapping);

    // Coverage: at least one mapping entry per acceptance criterion (or, when
    // the issue declared none, at least one entry overall).
    let required = acceptance.len().max(1);
    if entries.len() < required {
        findings.push(format!(
            "Only {} mapping entr{} for {} acceptance criteri{} -- map every criterion.",
            entries.len(),
            if entries.len() == 1 { "y" } else { "ies" },
            acceptance.len(),
            if acceptance.len() == 1 { "on" } else { "a" },
        ));
    }

    // Per-entry substance + evidence.
    for (i, entry) in entries.iter().enumerate() {
        let prose = strip_evidence_lines(entry);
        let words = prose.split_whitespace().count();
        if words < MIN_MAPPING_WORDS {
            findings.push(format!(
                "Mapping entry {} is too thin ({} words) -- explain how the change satisfies the \
                 criterion, do not restate it.",
                i + 1,
                words,
            ));
        }
        if !has_evidence(entry) {
            findings.push(format!(
                "Mapping entry {} cites no evidence -- name a test, a file:line, or an observable \
                 behavior (an `Evidence:` line).",
                i + 1,
            ));
        }
    }

    match not_done {
        None => findings.push(NOT_DONE_MISSING.to_owned()),
        Some(c) if c.split_whitespace().count() < 3 => findings.push(
            "The 'Not done' section is empty -- state what was deliberately left out, or 'nothing'."
                .to_owned(),
        ),
        Some(_) => {}
    }

    PrWriteReport {
        ok: findings.is_empty(),
        findings,
        mapping_entries: entries.len(),
    }
}

/// Split a mapping section's content into per-criterion entries on `### `
/// subheadings. Text before the first `### ` is ignored (it is preamble, not
/// an entry). Each returned string includes the subheading line and its body.
fn split_entries(mapping: &str) -> Vec<String> {
    let mut entries: Vec<String> = Vec::new();
    let mut current: Option<String> = None;

    for line in mapping.lines() {
        if line.trim_start().starts_with("### ") {
            if let Some(prev) = current.take() {
                entries.push(prev.trim().to_string());
            }
            current = Some(format!("{line}\n"));
        } else if let Some(buf) = current.as_mut() {
            buf.push_str(line);
            buf.push('\n');
        }
    }
    if let Some(last) = current {
        entries.push(last.trim().to_string());
    }
    entries
}

/// Strip the `### ` heading and any `Evidence:`-prefixed line from an entry,
/// leaving the explanatory prose to be word-counted. The heading restates the
/// criterion and the evidence line is checked separately, so neither should
/// count toward the substance threshold.
///
/// Shared with `simplify_check`, which uses the same strip-then-count logic
/// to validate per-file simplify articulation entries.
pub(crate) fn strip_evidence_lines(entry: &str) -> String {
    entry
        .lines()
        .filter(|l| {
            let t = l.trim_start();
            !t.starts_with("### ") && !t.to_lowercase().starts_with("evidence:")
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// True when a PR mapping entry cites evidence. Deliberately generous: a located
/// construct (`Evidence:` line, `fn`/`::` symbol, or a source path -- bare or
/// with a line) OR a behavioral cue word, since a PR mapping may legitimately
/// cite "observable behavior changed" as evidence that an acceptance criterion
/// is met. The goal is to confirm the agent pointed at something checkable, not
/// to validate the citation itself (that is the verify gate's job). The simplify
/// gate does NOT use this -- it uses the stricter `has_within_file_locator`.
pub(crate) fn has_evidence(entry: &str) -> bool {
    let lower = entry.to_lowercase();
    if lower.contains("evidence:")
        || entry.contains("::")
        || entry.contains("fn ")
        || has_source_path(entry)
    {
        return true;
    }
    // Behavioral cues, matched as whole words so "latest"/"fastest" don't
    // count as "test" and a stray "testing" prose word doesn't slip through.
    lower.split(|c: char| !c.is_alphanumeric()).any(|w| {
        matches!(
            w,
            "test" | "tests" | "behavior" | "behaviour" | "behavioral"
        )
    })
}

/// True when an entry cites a **within-file locator**: an explicit `Evidence:`
/// line, a `fn`/`::` symbol, a source path carrying a line number (`foo.rs:42`),
/// or a bare line reference (`line 42` / `lines 40 and 92`). Stricter than
/// `has_evidence`'s located branch in two ways: a behavioral cue word does not
/// count, and a BARE filename does not count.
///
/// The simplify gate uses this (read against the entry body). The `### <path>`
/// heading already names the file, so restating "src/foo.rs reads cleanly" in
/// prose is not a locator -- the agent must point at a line or a symbol it
/// actually read. `pub(crate)` so the two gates share one tokenizer and the
/// strict bar cannot silently drift from the generous one.
pub(crate) fn has_within_file_locator(entry: &str) -> bool {
    let lower = entry.to_lowercase();
    if lower.contains("evidence:") {
        return true;
    }
    entry.contains("::")
        || entry.contains("fn ")
        || has_source_path_with_line(entry)
        || has_line_reference(entry)
}

/// True when the entry cites a line within the file in natural prose --
/// "line 42", "lines 40 and 92". The `### <path>` heading already names the
/// file, so a line number alone is a within-file locator. Accepting this keeps
/// the natural review-prose style ("the duplication at lines 40 and 92") valid
/// without forcing the joined `foo.rs:42` form.
fn has_line_reference(entry: &str) -> bool {
    let toks: Vec<&str> = entry.split_whitespace().collect();
    toks.windows(2).any(|w| {
        let kw = w[0].trim_end_matches([':', '.', ',']).to_lowercase();
        (kw == "line" || kw == "lines")
            && w[1]
                .trim_start_matches(['(', '[', '#'])
                .chars()
                .next()
                .is_some_and(|c| c.is_ascii_digit())
    })
}

/// Source-file extensions recognized in evidence prose.
const SOURCE_EXTS: [&str; 12] = [
    ".rs", ".ts", ".tsx", ".js", ".py", ".go", ".sh", ".md", ".toml", ".json", ".sql", ".rb",
];

/// Whitespace tokens with trailing punctuation stripped, so "src/foo.rs." and
/// "(bar.ts:42)" still match. Shared by the path detectors.
fn source_tokens(entry: &str) -> impl Iterator<Item = &str> {
    entry
        .split_whitespace()
        .map(|raw| raw.trim_end_matches([',', '.', ';', ')', ']']))
}

/// True when the entry mentions a file with a recognized source extension --
/// the token ends with the extension (`foo.rs`) or carries a line suffix
/// (`bar.ts:42`). A bare substring match would fire on prose like "command".
fn has_source_path(entry: &str) -> bool {
    source_tokens(entry).any(|tok| {
        SOURCE_EXTS
            .iter()
            .any(|ext| tok.ends_with(ext) || tok.contains(&format!("{ext}:")))
    })
}

/// True when the entry mentions a source path WITH a real line number
/// (`bar.ts:42`). A bare filename (`bar.ts`) and an empty line suffix
/// (`bar.ts:`) both fail -- the colon must be followed by a digit. This is the
/// within-file half of the path check, used by the stricter simplify locator.
fn has_source_path_with_line(entry: &str) -> bool {
    source_tokens(entry).any(|tok| {
        SOURCE_EXTS.iter().any(|ext| {
            let needle = format!("{ext}:");
            tok.match_indices(&needle).any(|(i, _)| {
                tok[i + needle.len()..]
                    .chars()
                    .next()
                    .is_some_and(|c| c.is_ascii_digit())
            })
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn good_body() -> String {
        "## Summary\n\nDoes the thing.\n\n\
         ## Acceptance criteria mapping\n\n\
         ### 1. Cards default to Backlog\n\
         insert_card now binds the status column to a CardStatus param instead of \
         hardcoding pending, so freshly created and synced cards land in Backlog.\n\
         Evidence: tests/integration.rs::born_backlog_default\n\n\
         ### 2. Assign promotes to Pending\n\
         The only transition into Pending is Action::Assign, which the FSM enforces; \
         every other creation path stays in Backlog.\n\
         Evidence: src/kanban.rs:120 transition table\n\n\
         ## Not done\n\n\
         Did not migrate existing pending rows -- out of scope, tracked separately.\n"
            .to_owned()
    }

    #[test]
    fn accepts_a_substantive_mapping() {
        let ac = vec![
            "Cards default to Backlog".to_owned(),
            "Assign promotes to Pending".to_owned(),
        ];
        let report = validate_pr_body(&ac, &good_body());
        assert!(report.ok, "expected clean, got {:?}", report.findings);
        assert_eq!(report.mapping_entries, 2);
    }

    #[test]
    fn refuses_missing_mapping_section() {
        let ac = vec!["Something".to_owned()];
        let body = "## Summary\n\nDid stuff.\n\n## Not done\n\nNothing skipped here really.\n";
        let report = validate_pr_body(&ac, body);
        assert!(!report.ok);
        assert!(report.findings.iter().any(|f| f.contains("mapping")));
    }

    #[test]
    fn refuses_missing_not_done_section() {
        let ac = vec!["Something".to_owned()];
        let body = "## Acceptance criteria mapping\n\n\
                    ### 1. Something\n\
                    The change wires the handler through the dispatch table and covers it.\n\
                    Evidence: foo.rs::handles_something\n";
        let report = validate_pr_body(&ac, body);
        assert!(!report.ok);
        assert!(report.findings.iter().any(|f| f.contains("Not done")));
    }

    #[test]
    fn refuses_boilerplate_restatement() {
        // Entry just restates the AC -- under the word threshold, no evidence.
        let ac = vec!["Cards default to Backlog".to_owned()];
        let body = "## Acceptance criteria mapping\n\n\
                    ### 1. Cards default to Backlog\n\
                    Cards default to Backlog.\n\n\
                    ## Not done\n\nNothing was skipped.\n";
        let report = validate_pr_body(&ac, body);
        assert!(!report.ok);
        assert!(report.findings.iter().any(|f| f.contains("too thin")));
        assert!(report.findings.iter().any(|f| f.contains("no evidence")));
    }

    #[test]
    fn refuses_under_coverage() {
        // Two ACs, one mapping entry.
        let ac = vec!["First criterion".to_owned(), "Second criterion".to_owned()];
        let body = "## Acceptance criteria mapping\n\n\
                    ### 1. First criterion\n\
                    The dispatch path now threads the flag through and the handler honors it.\n\
                    Evidence: foo.rs::first\n\n\
                    ## Not done\n\nSecond criterion deferred.\n";
        let report = validate_pr_body(&ac, body);
        assert!(!report.ok);
        assert!(
            report
                .findings
                .iter()
                .any(|f| f.contains("map every criterion"))
        );
    }

    #[test]
    fn requires_one_entry_even_without_declared_acceptance() {
        // Issue declared no AC; the forcing function must not become a no-op.
        let ac: Vec<String> = vec![];
        // Mapping section present (non-empty so it survives parsing) but with
        // no `### ` entries -- coverage still requires at least one.
        let empty =
            "## Acceptance criteria mapping\n\nSee below.\n\n## Not done\n\nNothing skipped.\n";
        let report = validate_pr_body(&ac, empty);
        assert!(!report.ok);
        assert!(
            report
                .findings
                .iter()
                .any(|f| f.contains("map every criterion"))
        );
    }

    #[test]
    fn evidence_detected_in_several_forms() {
        assert!(has_evidence("Evidence: see the new behavior"));
        assert!(has_evidence("covered by foo::bar"));
        assert!(has_evidence("added fn validate_thing"));
        assert!(has_evidence("see src/pr_write.rs for the change"));
        assert!(has_evidence("observable behavior changed"));
        assert!(!has_evidence("just words with no citation at all here"));
        // Word-boundary: "latest"/"fastest"/"greatest" embed "test" but are
        // not evidence, and ".md" inside a prose word is not a source path.
        assert!(!has_evidence(
            "the latest greatest fastest command we shipped"
        ));
        assert!(has_evidence("see src/pr_write.rs."));
        assert!(has_evidence("the change at main.rs:5763 wires both gates"));
    }

    #[test]
    fn within_file_locator_excludes_bare_path_and_behavioral() {
        // Within-file locators: symbol, path:line, or an Evidence: line.
        assert!(has_within_file_locator("see src/foo.rs:12"));
        assert!(has_within_file_locator("covered by foo::bar"));
        assert!(has_within_file_locator("added fn validate_thing"));
        assert!(has_within_file_locator("Evidence: the lone match arm"));
        // Bare line references in natural prose are within-file locators (the
        // heading names the file), so the review-prose style stays valid.
        assert!(has_within_file_locator(
            "the duplication at lines 40 and 92"
        ));
        assert!(has_within_file_locator("removed the dead branch at line 7"));
        // NOT a within-file locator: a bare filename (which just repeats the
        // simplify heading), an empty line suffix, "line" without a number, or a
        // behavioral cue word.
        assert!(!has_within_file_locator("src/foo.rs reads cleanly"));
        assert!(!has_within_file_locator("src/foo.rs: reads cleanly"));
        assert!(!has_within_file_locator("lines of boilerplate everywhere"));
        assert!(!has_within_file_locator(
            "the observable behavior is unchanged"
        ));
        assert!(!has_within_file_locator("tested manually, looks clean"));
        // has_evidence stays generous for pr-write: a bare path and a
        // behavioral cue both still count.
        assert!(has_evidence("src/foo.rs reads cleanly"));
        assert!(has_evidence("the observable behavior is unchanged"));
    }
}
