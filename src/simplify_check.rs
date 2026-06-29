// Simplify articulation validator (#665).
//
// `legion quality-gate check` is the forcing-function analog of `legion pr
// write-check` for the simplify gate. Before `legion quality-gate record
// --skill legion-simplify` can record a clean gate, the agent must produce a
// structured articulation that this module validates.
//
// The lifetime audit (2026-06-18) showed legion-simplify has a 0.66% catch
// rate -- 7 issues in 1062 runs. The mechanism mirrors what makes pr-write
// work at 23.5%: require structured prose (one entry per changed file),
// validate it, and refuse boilerplate. Articulation is verification: composing
// the file-by-file analysis forces the agent to re-read the diff.
//
// The articulation file is a markdown document with one `### <path>` heading
// per changed file followed by prose explaining the simplify checks applied
// and the clean-or-finding verdict with reasoning. This module validates that
// every changed file is covered and each entry's prose is substantive.

use std::collections::HashSet;

use crate::pr_write::{MIN_MAPPING_WORDS, has_within_file_locator, strip_evidence_lines};

/// Minimum words of prose per file entry, after the heading is stripped.
/// Aliased to `pr_write::MIN_MAPPING_WORDS` so both gates share a single
/// source of truth for the substance bar and cannot silently drift. A genuine
/// per-file analysis clears this easily; a restatement of category names does not.
const MIN_ENTRY_WORDS: usize = MIN_MAPPING_WORDS;

/// A parsed entry from a simplify articulation document.
#[derive(Debug, Clone)]
pub struct ArticulationEntry {
    /// The file path from the `### <path>` heading.
    pub file_path: String,
    /// The full entry text, including the heading line.
    pub raw: String,
}

/// Result of validating a simplify articulation against a changed-file set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SimplifyCheckReport {
    /// True when every file is covered with substantive prose.
    pub ok: bool,
    /// Human-readable gaps, one per failed requirement. Empty when `ok`.
    pub findings: Vec<String>,
    /// Number of file entries found in the articulation.
    pub entry_count: usize,
}

/// Parse a simplify articulation document into per-file entries.
///
/// Splits on `### ` subheadings. Text before the first `### ` is ignored
/// (treated as preamble). Each returned entry carries the heading path and the
/// full raw text (heading + body).
pub fn parse_articulation(articulation: &str) -> Vec<ArticulationEntry> {
    let mut entries: Vec<ArticulationEntry> = Vec::new();
    let mut current_path: Option<String> = None;
    let mut current_buf: String = String::new();

    for line in articulation.lines() {
        if line.trim_start().starts_with("### ") {
            if let Some(path) = current_path.take() {
                entries.push(ArticulationEntry {
                    file_path: path,
                    raw: current_buf.trim().to_string(),
                });
            }
            // Extract the path from the heading. Strip leading `### ` and
            // any trailing whitespace. A heading like `### src/foo.rs` yields
            // `src/foo.rs`.
            let path = line.trim_start_matches('#').trim().to_string();
            current_path = Some(path.clone());
            current_buf = format!("{line}\n");
        } else if current_path.is_some() {
            current_buf.push_str(line);
            current_buf.push('\n');
        }
    }
    if let Some(path) = current_path {
        entries.push(ArticulationEntry {
            file_path: path,
            raw: current_buf.trim().to_string(),
        });
    }
    entries
}

/// Strip the `### <path>` heading line(s) from an articulation entry, leaving
/// the body for the located-evidence check. Unlike `strip_evidence_lines` this
/// KEEPS any `Evidence:` line (so an explicit citation counts) and removes only
/// the heading -- the heading restates the file path and would otherwise
/// satisfy `has_evidence`'s source-path test for free.
fn entry_body(raw: &str) -> String {
    raw.lines()
        .filter(|l| !l.trim_start().starts_with("### "))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Validate a simplify articulation against the set of changed files.
///
/// `changed_files` is the set of paths from
/// `git -c core.quotePath=false diff --name-only main...HEAD`.
/// `articulation` is the markdown text produced by the skill. Returns a
/// `SimplifyCheckReport` that names every coverage and substance gap found.
///
/// Validation rules:
/// 1. Coverage: every path in `changed_files` must have a `### <path>` entry.
///    A missing entry is reported by name.
/// 2. Substance: each entry's prose (after the heading is stripped) must
///    contain at least `MIN_ENTRY_WORDS` words. A thin entry that only
///    restates the category list is refused.
/// 3. No boilerplate: entries that just list the check categories without
///    reasoning are refused by the word threshold.
/// 4. Located evidence: each entry must point at the construct it examined --
///    a symbol (`fn name` / `::path`), a `file:line`, or an `Evidence:` line --
///    via `pr_write::has_within_file_locator`. Neither a behavioral cue word
///    nor a bare filename clears this bar (the bare filename just repeats the
///    heading). The check reads the entry body only: the `### <path>` heading
///    restates the file path and would otherwise satisfy the check for free.
///    A vague "looks clean" with no citation is exactly the rubber-stamp this
///    closes (legion-simplify caught 0.9% vs pr-write's 21.3%, the lone
///    structural difference being pr-write's evidence requirement).
pub fn validate_articulation(
    changed_files: &HashSet<String>,
    articulation: &str,
) -> SimplifyCheckReport {
    let mut findings: Vec<String> = Vec::new();
    let entries = parse_articulation(articulation);

    // Index entries by file path for O(1) lookup.
    let mut covered: HashSet<&str> = HashSet::new();
    for entry in &entries {
        covered.insert(entry.file_path.as_str());
    }

    // Coverage check: every changed file must have an entry.
    let mut missing: Vec<&str> = changed_files
        .iter()
        .filter(|f| !covered.contains(f.as_str()))
        .map(String::as_str)
        .collect();
    missing.sort_unstable();
    for path in &missing {
        findings.push(format!(
            "missing coverage: no articulation entry for changed file '{path}'"
        ));
    }

    // Substance check: each entry must have enough prose.
    // strip_evidence_lines already drops `### ` heading lines (they start with
    // "### "), so the heading path tokens do not count toward the word
    // threshold -- only the body prose does.
    for entry in &entries {
        let prose = strip_evidence_lines(&entry.raw);
        let words = prose.split_whitespace().count();
        if words < MIN_ENTRY_WORDS {
            findings.push(format!(
                "entry '{}' is too thin ({} words) -- explain which checks were \
                 applied and the reasoning for the clean-or-finding verdict, \
                 do not just list category names.",
                entry.file_path, words,
            ));
        }

        // Located-evidence check. Read the body only -- the `### <path>`
        // heading always contains the file path and would satisfy the
        // source-path test for free, defeating the requirement. Uses the
        // strict `has_within_file_locator` (symbol / file:line / Evidence:
        // line) -- neither a behavioral cue word nor a BARE filename (which
        // just repeats the heading) clears this structural gate.
        if !has_within_file_locator(&entry_body(&entry.raw)) {
            findings.push(format!(
                "entry '{}' cites no located evidence -- point at the construct \
                 you examined (a file:line, a `fn`/`::` symbol, or an \
                 `Evidence:` line), not just the file name.",
                entry.file_path,
            ));
        }
    }

    // When there are no changed files and no entries, the articulation is
    // vacuously valid (e.g. a docs-only commit with no source files). Report
    // clean without requiring entries.
    SimplifyCheckReport {
        ok: findings.is_empty(),
        findings,
        entry_count: entries.len(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn changed(paths: &[&str]) -> HashSet<String> {
        paths.iter().map(|s| s.to_string()).collect()
    }

    fn good_articulation() -> String {
        "# Simplify articulation\n\n\
         ### src/foo.rs\n\
         Checked for duplicate logic, unnecessary abstraction, stringly-typed state, \
         hand-rolled standard library dupes, copy-paste variation, and error swallowing. \
         No issues found: `fn parse_foo` at src/foo.rs:30 has a single responsibility and \
         propagates errors via the `?` operator throughout, so there is nothing to extract.\n\n\
         ### src/bar.rs\n\
         Reviewed for duplicate logic clusters and abstraction altitude. Found one \
         instance where two adjacent match arms share a body -- extracted into a helper \
         `fn apply_default` at line 42. The remaining logic is clean: no stringly-typed \
         state, no hand-rolled stdlib dupes, no error swallowing detected.\n"
            .to_string()
    }

    #[test]
    fn accepts_substantive_articulation() {
        let files = changed(&["src/foo.rs", "src/bar.rs"]);
        let report = validate_articulation(&files, &good_articulation());
        assert!(report.ok, "expected clean, got {:?}", report.findings);
        assert_eq!(report.entry_count, 2);
    }

    #[test]
    fn refuses_missing_coverage() {
        let files = changed(&["src/foo.rs", "src/bar.rs", "src/baz.rs"]);
        let report = validate_articulation(&files, &good_articulation());
        assert!(!report.ok);
        assert!(
            report
                .findings
                .iter()
                .any(|f| f.contains("src/baz.rs") && f.contains("missing coverage")),
            "expected a missing-coverage finding naming src/baz.rs, got {:?}",
            report.findings
        );
    }

    #[test]
    fn refuses_boilerplate_thin_entry() {
        let files = changed(&["src/foo.rs"]);
        let thin = "### src/foo.rs\nLooks clean.\n";
        let report = validate_articulation(&files, thin);
        assert!(!report.ok);
        assert!(
            report
                .findings
                .iter()
                .any(|f| f.contains("too thin") && f.contains("src/foo.rs")),
            "expected a thin-entry finding, got {:?}",
            report.findings
        );
    }

    #[test]
    fn refuses_entry_without_located_evidence() {
        // Plenty of prose, but a vague "looks clean" with no file:line or
        // symbol -- exactly the rubber-stamp this gate now catches.
        let files = changed(&["src/foo.rs"]);
        let vague = "### src/foo.rs\n\
             Checked for duplicate logic, unnecessary abstraction, stringly-typed state, \
             hand-rolled stdlib dupes, copy-paste variation, and error swallowing. \
             Everything reads cleanly and there is nothing worth extracting or simplifying.\n";
        let report = validate_articulation(&files, vague);
        assert!(!report.ok);
        assert!(
            report
                .findings
                .iter()
                .any(|f| f.contains("no located evidence") && f.contains("src/foo.rs")),
            "expected a located-evidence finding for src/foo.rs, got {:?}",
            report.findings
        );
        // The body clears the word threshold -- the failure is evidence, not thinness.
        assert!(
            !report.findings.iter().any(|f| f.contains("too thin")),
            "evidence test should not also trip the thinness check: {:?}",
            report.findings
        );
    }

    #[test]
    fn refuses_entry_with_only_behavioral_cue() {
        // "behavior" is evidence for pr-write but NOT a located construct, so
        // the simplify gate must reject an entry whose only anchor is a cue
        // word. Regression guard for the HIGH the pre-push review caught: the
        // gate must enforce the located construct it advertises.
        let files = changed(&["src/foo.rs"]);
        let body = "### src/foo.rs\n\
             Reviewed all six categories across the module; the observable behavior is \
             unchanged and the structure reads cleanly with nothing worth extracting here.\n";
        let report = validate_articulation(&files, body);
        assert!(!report.ok);
        assert!(
            report
                .findings
                .iter()
                .any(|f| f.contains("no located evidence")),
            "behavioral cue must not satisfy the located-evidence bar, got {:?}",
            report.findings
        );
    }

    #[test]
    fn refuses_entry_with_only_bare_path() {
        // The `### <path>` heading already names the file; restating the bare
        // path in prose is not a within-file locator. (Pre-push review HIGH,
        // round 2 -- a bare filename must not clear the gate.)
        let files = changed(&["src/foo.rs"]);
        let body = "### src/foo.rs\n\
             Reviewed all six categories; src/foo.rs reads cleanly with focused, \
             single-purpose helpers and no duplication or stringly-typed state worth changing.\n";
        let report = validate_articulation(&files, body);
        assert!(!report.ok);
        assert!(
            report
                .findings
                .iter()
                .any(|f| f.contains("no located evidence")),
            "a bare filename must not satisfy the within-file locator, got {:?}",
            report.findings
        );
    }

    #[test]
    fn accepts_entry_with_explicit_evidence_line() {
        let files = changed(&["src/foo.rs"]);
        let body = "### src/foo.rs\n\
             Reviewed all six simplify categories across the module and found the logic \
             focused, with no duplication or abstraction-altitude problems worth changing.\n\
             Evidence: src/foo.rs:12 the single match in handle_event\n";
        let report = validate_articulation(&files, body);
        assert!(report.ok, "expected clean, got {:?}", report.findings);
    }

    #[test]
    fn vacuously_valid_when_no_changed_files() {
        let files: HashSet<String> = HashSet::new();
        let report = validate_articulation(&files, "# No changed files\n");
        assert!(report.ok);
        assert_eq!(report.entry_count, 0);
    }

    #[test]
    fn parse_articulation_extracts_entries() {
        let entries = parse_articulation(&good_articulation());
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].file_path, "src/foo.rs");
        assert_eq!(entries[1].file_path, "src/bar.rs");
        assert!(entries[0].raw.contains("single"));
    }

    #[test]
    fn multiple_missing_files_all_named() {
        let files = changed(&["src/a.rs", "src/b.rs", "src/c.rs"]);
        let articulation = "### src/a.rs\n\
             Checked all categories: duplicate logic, unnecessary abstraction, \
             stringly-typed state, hand-rolled std dupes, copy-paste, error swallowing. \
             All clear: single-purpose functions with explicit types throughout.\n";
        let report = validate_articulation(&files, articulation);
        assert!(!report.ok);
        // Both b.rs and c.rs should be named.
        let gap_count = report
            .findings
            .iter()
            .filter(|f| f.contains("missing coverage"))
            .count();
        assert_eq!(
            gap_count, 2,
            "expected 2 missing-coverage findings, got {:?}",
            report.findings
        );
    }

    #[test]
    fn mixed_gaps_names_all() {
        // Two changed files: one covered thinly, one missing entirely.
        let files = changed(&["src/foo.rs", "src/bar.rs"]);
        let articulation = "### src/foo.rs\nThin.\n";
        let report = validate_articulation(&files, articulation);
        assert!(!report.ok);
        let has_thin = report.findings.iter().any(|f| f.contains("too thin"));
        let has_missing = report
            .findings
            .iter()
            .any(|f| f.contains("missing coverage") && f.contains("src/bar.rs"));
        assert!(has_thin, "expected thin-entry finding");
        assert!(has_missing, "expected missing-coverage finding for bar.rs");
    }

    #[test]
    fn findings_vec_reflects_gaps() {
        let report = SimplifyCheckReport {
            ok: false,
            findings: vec!["gap one".to_string(), "gap two".to_string()],
            entry_count: 1,
        };
        assert_eq!(report.findings.len(), 2);
    }

    #[test]
    fn findings_vec_empty_on_clean() {
        let report = SimplifyCheckReport {
            ok: true,
            findings: vec![],
            entry_count: 3,
        };
        assert_eq!(report.findings.len(), 0);
    }
}
