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

use crate::pr_write::{MIN_MAPPING_WORDS, has_within_file_locator};

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
/// the body for both the substance count and the located-evidence check.
/// Unlike `pr_write::strip_evidence_lines` this KEEPS any `Evidence:` line (so
/// an explicit citation counts toward both the word threshold and
/// `has_within_file_locator`'s source-path test) and removes only the heading
/// -- the heading restates the file path and would otherwise satisfy that
/// test for free.
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

    // Both checks below read the same stripped body -- see `entry_body`'s doc
    // comment for why it (not `pr_write::strip_evidence_lines`) is the right
    // stripper for simplify: an `Evidence:` line is simplify's own
    // within-file locator, not a foreign convention to discard.
    for entry in &entries {
        let body = entry_body(&entry.raw);

        // Substance check: each entry must have enough prose.
        let words = body.split_whitespace().count();
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
        if !has_within_file_locator(&body) {
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
    fn accepts_entry_whose_substance_is_on_the_evidence_line() {
        // Regression guard for the strip_evidence_lines convention leak (#669):
        // simplify_check must not reuse pr_write's evidence-line-dropping
        // stripper for the word count. An entry that packs its reasoning onto
        // the `Evidence:` line must still count those words toward the
        // substance threshold rather than losing them silently.
        let files = changed(&["src/foo.rs"]);
        let body = "### src/foo.rs\n\
             Evidence: src/foo.rs:12 fn handle_event has a single match arm per variant, \
             propagates errors via `?`, and duplicates nothing worth extracting here.\n";
        let report = validate_articulation(&files, body);
        assert!(
            report.ok,
            "prose on the Evidence: line must count toward substance, got {:?}",
            report.findings
        );
    }

    #[test]
    fn refuses_rename_when_old_path_entry_missing() {
        // `git_changed_files` now runs `git diff --name-status -M50%`
        // (#779), not `--name-only`: a rename detected at or above the
        // pinned 50% similarity surfaces as a single `R<nn>\t<old>\t<new>`
        // line, and `git_changed_files` puts the R100 (zero-delta) case
        // through R100 auto-clear or, for R<100 (content delta), just the
        // new path into the coverage set. A rename that falls BELOW the
        // pinned similarity is still undetected as a rename at the git
        // level and is reported as two independent lines -- a delete of the
        // old path and an add of the new path -- exactly as it was under
        // `--name-only`. This test constructs that latter (still-relevant)
        // case directly against `validate_articulation`, which is agnostic
        // to where the changed-file set came from: it only sees two
        // unrelated paths in the set. Coverage must be exact-path: an
        // articulation addressing only the new path still leaves the old
        // path uncovered, and the gate refuses by name -- an agent cannot
        // clear this by adding more prose about the new path, since the old
        // path simply will not match.
        let files = changed(&["src/old_name.rs", "src/new_name.rs"]);
        let only_new_path = "### src/new_name.rs\n\
             Renamed from old_name.rs and reviewed for duplicate logic, stringly-typed \
             state, and unnecessary abstraction. `fn parse` at line 10 is unchanged \
             behaviorally aside from the move; nothing to extract.\n";
        let report = validate_articulation(&files, only_new_path);
        assert!(
            !report.ok,
            "expected a refusal for the uncovered old path, got clean"
        );
        assert!(
            report
                .findings
                .iter()
                .any(|f| f.contains("missing coverage") && f.contains("src/old_name.rs")),
            "expected a missing-coverage finding naming the old path exactly, got {:?}",
            report.findings
        );

        // Only when BOTH git-reported paths get an exact-match entry does the
        // articulation pass -- there is no way to satisfy coverage for the
        // old path other than heading it with the exact path git reports.
        let both_paths = format!(
            "{only_new_path}\n### src/old_name.rs\n\
             Deleted as part of the rename to new_name.rs; the content now lives there and \
             was reviewed under that heading, so nothing further to check under this path.\n\
             Evidence: content moved to src/new_name.rs, reviewed there in full\n"
        );
        let report = validate_articulation(&files, &both_paths);
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
