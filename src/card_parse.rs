// Parse a markdown issue body into structured fields.
// Extracts sections by `## Heading` markers. Recognizes common template
// headings (Problem, Solution, Acceptance criteria, Implementation) and
// falls back to generic heading-keyed sections for non-template issues.

/// Structured representation of a parsed issue body.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct ParsedIssue {
    /// Short problem statement (first paragraph of ## Problem)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub problem: Option<String>,
    /// Solution summary (first paragraph of ## Solution)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub solution: Option<String>,
    /// Acceptance criteria as individual items
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub acceptance: Vec<String>,
    /// Bullets from `## Traces to` (#933): which requirement(s) this issue
    /// derives from, and which of their criteria it services. Empty when
    /// the section is absent -- an untraced issue is legal (only new work
    /// has a spec; defect work anchors on its stated premise instead).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub trace: Vec<TraceBullet>,
    /// Other sections keyed by heading name
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub sections: Vec<(String, String)>,
    /// Raw body fallback when no headings found
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
}

/// One bullet under an issue's `## Traces to` section (#933 trace format
/// contract -- this is the single definition both the parser and the issue
/// template are written against). Either a reference to a requirement, or
/// the explicit no-requirement spelling.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum TraceBullet {
    /// `- FR-XXXX-NNN -- <prose>` and/or `- FR-XXXX-NNN [criteria: <id>, <id>]`.
    Requirement {
        /// First token of the bullet: the requirement document's id.
        document_id: String,
        /// Cited criterion ids from an optional `[criteria: ...]` bracket.
        /// `None` means the bracket was absent -- the whole requirement is
        /// in scope for this issue, per the contract.
        #[serde(skip_serializing_if = "Option::is_none")]
        criteria: Option<Vec<String>>,
        /// Free text after ` -- `, for humans; not parsed further.
        #[serde(skip_serializing_if = "Option::is_none")]
        prose: Option<String>,
    },
    /// `- None -- <reason>`, the explicit no-requirement case. `reason` is
    /// `None` when the bullet omitted it -- a structural gap the caller
    /// refuses on (`cli::issue`'s create-time validation), not something
    /// the parser fabricates or silently accepts.
    NoRequirement {
        #[serde(skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
}

/// Parse a markdown issue body into structured fields.
pub fn parse_issue_body(body: &str) -> ParsedIssue {
    let body = body.trim();
    if body.is_empty() {
        return ParsedIssue::default();
    }

    let sections = extract_sections(body);

    if sections.is_empty() {
        return ParsedIssue {
            body: Some(body.to_string()),
            ..Default::default()
        };
    }

    let mut parsed = ParsedIssue::default();

    for (heading, content) in &sections {
        // Normalize before matching (#907): trailing punctuation on a heading
        // is a formatting choice, never a different section. `## Acceptance
        // criteria:` previously matched nothing, fell through to the generic
        // bucket, and yielded ZERO criteria -- which then silently relaxed the
        // pr-write gate. Strictness bought nothing here: there is no competing
        // heading a trailing colon could disambiguate against.
        let key = heading.to_lowercase();
        let key = key.trim_end_matches([':', '.', ' ']);
        let content = content.trim();
        if content.is_empty() {
            continue;
        }

        match key {
            "problem" | "bug" | "issue" => {
                parsed.problem = Some(first_paragraph(content));
            }
            "solution" | "fix" | "what" | "proposal" => {
                parsed.solution = Some(first_paragraph(content));
            }
            "acceptance criteria" | "acceptance" | "done when" | "done" => {
                // #961: extend, not assign. A body carrying both an
                // `## Acceptance criteria` and a `## Done When` heading (the
                // exact shape the view/edit round-trip bug produced, see
                // #947) used to clobber -- whichever heading extract_sections
                // visited second silently discarded the first's criteria.
                parsed.acceptance.extend(extract_checklist(content));
            }
            "traces to" => {
                parsed.trace = parse_trace_bullets(content);
            }
            _ => {
                parsed.sections.push((heading.clone(), content.to_string()));
            }
        }
    }

    parsed
}

/// Extract `## Heading` sections from markdown text.
/// Returns Vec of (heading, content) pairs.
fn extract_sections(text: &str) -> Vec<(String, String)> {
    let mut sections: Vec<(String, String)> = Vec::new();
    let mut current_heading: Option<String> = None;
    let mut current_content = String::new();

    for line in text.lines() {
        if let Some(heading) = line.strip_prefix("## ") {
            if let Some(prev_heading) = current_heading.take() {
                sections.push((prev_heading, current_content.trim().to_string()));
            }
            current_heading = Some(heading.trim().to_string());
            current_content = String::new();
        } else if current_heading.is_some() {
            current_content.push_str(line);
            current_content.push('\n');
        }
    }

    if let Some(heading) = current_heading {
        sections.push((heading, current_content.trim().to_string()));
    }

    sections
}

/// Split a raw body into its preamble (content before the first `## `
/// heading) and its heading-delimited sections, byte-for-byte (#961, for
/// `legion issue view --json`).
///
/// This is a separate function from `extract_sections`, not a shared
/// implementation: `extract_sections` trims each section's content for
/// `parse_issue_body`'s structured extraction, which is fine for that
/// consumer but is not byte-exact. `legion issue view --json`'s entire point
/// is losslessness, so this function never trims -- `content` keeps its
/// leading newline exactly when the heading line had one, rather than a
/// separate flag, so `preamble` followed by `"## " + heading + content` for
/// every section, in order, always reconstructs the original body exactly,
/// including the edge case of a final heading with no trailing newline at
/// all (content is `""` there, not `"\n"`, so no byte is invented).
pub fn split_body_lossless(body: &str) -> (String, Vec<(String, String)>) {
    let mut heading_starts: Vec<usize> = Vec::new();
    if body.starts_with("## ") {
        heading_starts.push(0);
    }
    let mut search_from = 0;
    while let Some(rel) = body[search_from..].find("\n## ") {
        let abs = search_from + rel + 1;
        heading_starts.push(abs);
        search_from = abs + 1;
    }

    if heading_starts.is_empty() {
        return (body.to_string(), Vec::new());
    }

    let preamble = body[..heading_starts[0]].to_string();
    let mut sections = Vec::with_capacity(heading_starts.len());
    for (i, &start) in heading_starts.iter().enumerate() {
        let end = heading_starts.get(i + 1).copied().unwrap_or(body.len());
        let after_prefix = &body[start + 3..end];
        let (heading, content) = match after_prefix.find('\n') {
            Some(nl) => (&after_prefix[..nl], &after_prefix[nl..]),
            None => (after_prefix, ""),
        };
        sections.push((heading.to_string(), content.to_string()));
    }

    (preamble, sections)
}

/// Get the first paragraph from a section (up to the first blank line).
fn first_paragraph(text: &str) -> String {
    let mut lines: Vec<&str> = Vec::new();
    for line in text.lines() {
        if line.trim().is_empty() && !lines.is_empty() {
            break;
        }
        if !line.trim().is_empty() {
            lines.push(line.trim());
        }
    }
    lines.join(" ")
}

/// Extract checklist items from markdown (- [ ] or - lines).
fn extract_checklist(text: &str) -> Vec<String> {
    text.lines()
        .filter_map(|line| {
            let trimmed = line.trim();
            if let Some(item) = trimmed.strip_prefix("- [ ] ") {
                Some(item.to_string())
            } else if let Some(item) = trimmed.strip_prefix("- [x] ") {
                Some(item.to_string())
            } else if let Some(item) = trimmed.strip_prefix("- [X] ") {
                Some(item.to_string())
            } else {
                trimmed.strip_prefix("- ").map(|item| item.to_string())
            }
        })
        .collect()
}

/// Parse the bullets under a `## Traces to` section (#933 trace format
/// contract). Tolerant of malformed lines -- a line that is not a `- `
/// bullet is skipped rather than erroring. Validating the parsed structure
/// (`None` beside an FR bullet, a missing `None` reason, an id that does not
/// resolve or a bracket citing a criterion the requirement lacks) is a
/// separate, document-store-touching step the caller performs at the point
/// it is needed (`cli::issue`'s create-time validation, `cli::verify`'s
/// trace resolution) -- this function only turns text into structure.
fn parse_trace_bullets(content: &str) -> Vec<TraceBullet> {
    content
        .lines()
        .filter_map(|line| line.trim().strip_prefix("- "))
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(parse_trace_bullet)
        .collect()
}

/// Parse one `## Traces to` bullet (its content after the leading `- ` has
/// already been stripped by `parse_trace_bullets`).
fn parse_trace_bullet(line: &str) -> TraceBullet {
    let mut parts = line.splitn(2, char::is_whitespace);
    let token = parts.next().unwrap_or("").trim();
    let mut remainder = parts.next().unwrap_or("").trim().to_string();

    if token == "None" {
        return TraceBullet::NoRequirement {
            reason: extract_trace_prose(&remainder),
        };
    }

    let criteria = extract_criteria_bracket(&mut remainder);
    let prose = extract_trace_prose(&remainder);

    TraceBullet::Requirement {
        document_id: token.to_string(),
        criteria,
        prose,
    }
}

/// Pull a `[criteria: id, id]` bracket out of a trace bullet's remainder (in
/// place), returning the parsed id list. Returns `None` (and leaves
/// `remainder` untouched) when no bracket is present -- the contract's
/// stated meaning of absence is "the whole requirement is in scope", not
/// "zero criteria", so this must stay distinguishable from `Some(vec![])`.
fn extract_criteria_bracket(remainder: &mut String) -> Option<Vec<String>> {
    let start = remainder.find("[criteria:")?;
    let end = start + remainder[start..].find(']')?;
    let inner = remainder[start + "[criteria:".len()..end].to_string();
    let ids: Vec<String> = inner
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect();

    let before = remainder[..start].trim();
    let after = remainder[end + 1..].trim();
    let mut combined = String::new();
    combined.push_str(before);
    if !before.is_empty() && !after.is_empty() {
        combined.push(' ');
    }
    combined.push_str(after);
    *remainder = combined;

    Some(ids)
}

/// Extract the human prose from what is left of a trace bullet after its id
/// token (and any `[criteria: ...]` bracket) has been removed. The contract
/// form is ` -- <prose>`; the leading `--` is stripped when present, but any
/// non-empty leftover text is still returned as prose (e.g. a bracket
/// followed directly by trailing text) rather than silently dropped.
fn extract_trace_prose(remainder: &str) -> Option<String> {
    let trimmed = remainder.trim();
    if trimmed.is_empty() {
        return None;
    }
    let stripped = trimmed.strip_prefix("--").map_or(trimmed, str::trim);
    if stripped.is_empty() {
        None
    } else {
        Some(stripped.to_string())
    }
}

/// Structural `[criteria: ...]` defects the tolerant parser cannot refuse
/// itself (#933, #945 review): the parse layer is infallible by design, so
/// a malformed bracket degrades -- an unclosed bracket lands unparsed in
/// `prose` and the bullet silently widens to whole-requirement scope; a
/// second bracket is dropped into `prose` and its ids never cited; an empty
/// bracket becomes `Some(vec![])`, a citation that scopes zero criteria and
/// no validator loop ever inspects. Each of those must be refused by the
/// gates, not passed through, so this check is defined once here -- beside
/// the parser whose tolerance creates the cases -- and called by both
/// `cli::issue::validate_trace` (create time) and
/// `cli::verify::resolve_traced_requirements` (live re-check).
///
/// Returns a human-readable defect description, or `None` for a
/// well-formed bullet. `NoRequirement` bullets have no bracket grammar and
/// always pass.
pub fn trace_bullet_bracket_defect(bullet: &TraceBullet) -> Option<String> {
    let TraceBullet::Requirement {
        document_id,
        criteria,
        prose,
    } = bullet
    else {
        return None;
    };

    if prose.as_deref().is_some_and(|p| p.contains("[criteria:")) {
        return Some(format!(
            "requirement '{document_id}' bullet carries an unparsed '[criteria:' fragment \
             (unclosed or repeated bracket) -- write exactly one '[criteria: id, id]' bracket"
        ));
    }
    if criteria.as_ref().is_some_and(|ids| ids.is_empty()) {
        return Some(format!(
            "'[criteria: ...]' for requirement '{document_id}' cites no ids -- omit the \
             bracket for whole-requirement scope, or name at least one id"
        ));
    }
    None
}

/// Truncate a string to at most `max` characters, appending `suffix` when
/// truncated. Safe for multi-byte UTF-8. When `max` is smaller than the
/// suffix, returns the leading `max` characters with no suffix -- there
/// isn't room to keep both content and the marker, and the naive
/// `max - suffix_len` underflowed on `usize`. Callers asking for a preview
/// shorter than the marker are getting a hard truncation by design (#346).
///
/// The crate-wide truncation primitive: `truncate_chars` is the "..." form,
/// usage's table columns use the "~" form.
pub fn truncate_chars_with(s: &str, max: usize, suffix: &str) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let suffix_len = suffix.chars().count();
    if max < suffix_len {
        return s.chars().take(max).collect();
    }
    let end: String = s.chars().take(max - suffix_len).collect();
    format!("{end}{suffix}")
}

/// Truncate a string to at most `max` characters, appending "..." if
/// truncated. See [`truncate_chars_with`] for the underflow guard (#346).
pub fn truncate_chars(s: &str, max: usize) -> String {
    truncate_chars_with(s, max, "...")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// #907: a trailing colon on the heading is a formatting choice, never a
    /// different section. It previously matched nothing, fell through to the
    /// generic bucket, and produced ZERO criteria -- which then silently
    /// relaxed the pr-write gate instead of failing anywhere visible. Five
    /// issues on smugglr's migrate spine were authored in the colon form.
    #[test]
    fn acceptance_heading_tolerates_trailing_punctuation() {
        for heading in [
            "## Acceptance criteria:",
            "## Acceptance criteria.",
            "## Acceptance criteria: ",
            "## Done when:",
            "## ACCEPTANCE CRITERIA:",
        ] {
            let body = format!("{heading}\n- Tests pass\n- Clippy clean\n");
            let parsed = parse_issue_body(&body);
            assert_eq!(
                parsed.acceptance,
                vec!["Tests pass".to_owned(), "Clippy clean".to_owned()],
                "heading {heading:?} must yield criteria, got {:?}",
                parsed.acceptance
            );
        }
    }

    /// Normalization must not swallow a genuinely different section -- only
    /// trailing punctuation is ignored, not surrounding words.
    #[test]
    fn acceptance_normalization_does_not_capture_other_headings() {
        let body = "## Acceptance criteria notes\n- Not a criterion\n";
        let parsed = parse_issue_body(body);
        assert!(
            parsed.acceptance.is_empty(),
            "a different heading must stay a generic section, got {:?}",
            parsed.acceptance
        );
        assert_eq!(parsed.sections.len(), 1);
    }

    #[test]
    fn truncate_chars_handles_max_less_than_ellipsis() {
        // #346: max < 3 used to underflow usize and panic. Now returns the
        // leading max characters as a hard truncation -- no ellipsis room.
        assert_eq!(truncate_chars("hello world", 0), "");
        assert_eq!(truncate_chars("hello world", 1), "h");
        assert_eq!(truncate_chars("hello world", 2), "he");
        assert_eq!(truncate_chars("hello world", 3), "...");
        assert_eq!(truncate_chars("hello world", 8), "hello...");
    }

    #[test]
    fn truncate_chars_passes_through_short_strings() {
        // No truncation when string fits.
        assert_eq!(truncate_chars("", 0), "");
        assert_eq!(truncate_chars("hi", 5), "hi");
        assert_eq!(truncate_chars("hello", 5), "hello");
    }

    #[test]
    fn truncate_chars_with_alternate_suffix() {
        // The "~" form used by usage's table columns: cap includes the marker.
        assert_eq!(truncate_chars_with("abcdefgh", 5, "~"), "abcd~");
        assert_eq!(truncate_chars_with("abc", 5, "~"), "abc");
        // Underflow guard (#346): no room for the marker -> hard truncation.
        assert_eq!(truncate_chars_with("abcdefgh", 0, "~"), "");
    }

    #[test]
    fn truncate_chars_is_unicode_safe() {
        // Multi-byte codepoints don't trip the truncation. Each emoji is
        // a single char, multiple bytes. With max=5 on a 6-char string,
        // takes (5-3=2) chars + ellipsis: "<emoji> ..."
        assert_eq!(truncate_chars("\u{1F4A1} idea", 5), "\u{1F4A1} ...");
        // max < 3 hard-truncates: 4 emojis, max=2 -> first 2 emojis.
        assert_eq!(
            truncate_chars("\u{1F4A1}\u{1F4A1}\u{1F4A1}\u{1F4A1}", 2),
            "\u{1F4A1}\u{1F4A1}"
        );
    }

    #[test]
    fn parse_template_issue() {
        let body = "## Problem\n\nThings are broken.\n\nMore detail here.\n\n## Solution\n\nFix them.\n\n## Acceptance criteria\n\n- [ ] Tests pass\n- [ ] Clippy clean\n";
        let parsed = parse_issue_body(body);
        assert_eq!(parsed.problem.as_deref(), Some("Things are broken."));
        assert_eq!(parsed.solution.as_deref(), Some("Fix them."));
        assert_eq!(parsed.acceptance, vec!["Tests pass", "Clippy clean"]);
        assert!(parsed.body.is_none());
    }

    #[test]
    fn parse_non_template_issue() {
        let body = "Just a plain text description with no headings.";
        let parsed = parse_issue_body(body);
        assert!(parsed.problem.is_none());
        assert!(parsed.body.as_deref() == Some("Just a plain text description with no headings."));
    }

    #[test]
    fn parse_empty_body() {
        let parsed = parse_issue_body("");
        assert!(parsed.problem.is_none());
        assert!(parsed.body.is_none());
    }

    #[test]
    fn parse_extra_sections() {
        let body = "## Problem\n\nBroken.\n\n## Implementation\n\nDo the thing.\n\n## Notes\n\nSome notes.\n";
        let parsed = parse_issue_body(body);
        assert_eq!(parsed.problem.as_deref(), Some("Broken."));
        assert_eq!(parsed.sections.len(), 2);
        assert_eq!(parsed.sections[0].0, "Implementation");
        assert_eq!(parsed.sections[1].0, "Notes");
    }

    #[test]
    fn parse_alternate_headings() {
        let body = "## Bug\n\nCrash on startup.\n\n## Fix\n\nHandle the null.\n\n## Done When\n\n- No crash\n- Tests pass\n";
        let parsed = parse_issue_body(body);
        assert_eq!(parsed.problem.as_deref(), Some("Crash on startup."));
        assert_eq!(parsed.solution.as_deref(), Some("Handle the null."));
        assert_eq!(parsed.acceptance, vec!["No crash", "Tests pass"]);
    }

    #[test]
    fn first_paragraph_extracts_correctly() {
        let text = "First line.\nSecond line.\n\nSecond paragraph.";
        assert_eq!(first_paragraph(text), "First line. Second line.");
    }

    // -- #933: `## Traces to` trace-format contract -------------------------

    #[test]
    fn trace_absent_when_no_traces_to_section() {
        let body = "## Problem\n\nSomething.\n";
        let parsed = parse_issue_body(body);
        assert!(parsed.trace.is_empty());
    }

    #[test]
    fn trace_parses_requirement_bullet_with_prose() {
        let body = "## Traces to\n\n- FR-EMAIL-003 -- adds the retry path\n";
        let parsed = parse_issue_body(body);
        assert_eq!(
            parsed.trace,
            vec![TraceBullet::Requirement {
                document_id: "FR-EMAIL-003".to_owned(),
                criteria: None,
                prose: Some("adds the retry path".to_owned()),
            }]
        );
    }

    #[test]
    fn trace_parses_requirement_bullet_with_criteria_bracket() {
        let body = "## Traces to\n\n- FR-EMAIL-003 [criteria: crit-1, crit-2]\n";
        let parsed = parse_issue_body(body);
        assert_eq!(
            parsed.trace,
            vec![TraceBullet::Requirement {
                document_id: "FR-EMAIL-003".to_owned(),
                criteria: Some(vec!["crit-1".to_owned(), "crit-2".to_owned()]),
                prose: None,
            }]
        );
    }

    #[test]
    fn trace_parses_bracket_and_prose_combined() {
        let body = "## Traces to\n\n- FR-EMAIL-003 [criteria: crit-1] -- also fixes the timeout\n";
        let parsed = parse_issue_body(body);
        assert_eq!(
            parsed.trace,
            vec![TraceBullet::Requirement {
                document_id: "FR-EMAIL-003".to_owned(),
                criteria: Some(vec!["crit-1".to_owned()]),
                prose: Some("also fixes the timeout".to_owned()),
            }]
        );
    }

    #[test]
    fn trace_absent_criteria_bracket_means_whole_requirement_in_scope() {
        let body = "## Traces to\n\n- FR-EMAIL-003 -- no bracket here\n";
        let parsed = parse_issue_body(body);
        match &parsed.trace[0] {
            TraceBullet::Requirement { criteria, .. } => {
                assert!(
                    criteria.is_none(),
                    "absent bracket must be None, not Some(vec![])"
                );
            }
            other => panic!("expected Requirement, got {other:?}"),
        }
    }

    #[test]
    fn trace_parses_multiple_requirement_bullets() {
        // #933: "an issue may service two requirements".
        let body = "## Traces to\n\n- FR-EMAIL-003 -- first\n- FR-EMAIL-004 -- second\n";
        let parsed = parse_issue_body(body);
        assert_eq!(parsed.trace.len(), 2);
        assert_eq!(
            parsed.trace[0],
            TraceBullet::Requirement {
                document_id: "FR-EMAIL-003".to_owned(),
                criteria: None,
                prose: Some("first".to_owned()),
            }
        );
        assert_eq!(
            parsed.trace[1],
            TraceBullet::Requirement {
                document_id: "FR-EMAIL-004".to_owned(),
                criteria: None,
                prose: Some("second".to_owned()),
            }
        );
    }

    #[test]
    fn trace_parses_none_bullet_with_reason() {
        let body = "## Traces to\n\n- None -- defect fix, no requirement above it\n";
        let parsed = parse_issue_body(body);
        assert_eq!(
            parsed.trace,
            vec![TraceBullet::NoRequirement {
                reason: Some("defect fix, no requirement above it".to_owned()),
            }]
        );
    }

    #[test]
    fn trace_parses_none_bullet_missing_reason_as_none() {
        // The parser is tolerant -- a missing reason is a structural gap the
        // create-time validator refuses on, not something parsing invents.
        let body = "## Traces to\n\n- None\n";
        let parsed = parse_issue_body(body);
        assert_eq!(
            parsed.trace,
            vec![TraceBullet::NoRequirement { reason: None }]
        );
    }

    #[test]
    fn bracket_defect_flags_unclosed_repeated_and_empty_brackets() {
        // Unclosed: bracket text degrades into prose, scope silently widens.
        let unclosed = &parse_issue_body("## Traces to\n\n- FR-X [criteria: a -- oops\n").trace[0];
        assert!(
            trace_bullet_bracket_defect(unclosed)
                .is_some_and(|m| m.contains("unparsed '[criteria:'")),
            "unclosed bracket must be flagged"
        );

        // Repeated: second bracket's ids silently dropped into prose.
        let repeated =
            &parse_issue_body("## Traces to\n\n- FR-X [criteria: a] [criteria: b]\n").trace[0];
        assert!(
            trace_bullet_bracket_defect(repeated).is_some(),
            "repeated bracket must be flagged"
        );

        // Empty: a citation scoping zero criteria.
        let empty = &parse_issue_body("## Traces to\n\n- FR-X [criteria:]\n").trace[0];
        assert!(
            trace_bullet_bracket_defect(empty).is_some_and(|m| m.contains("cites no ids")),
            "empty bracket must be flagged"
        );

        // Well-formed and bracketless bullets pass; None bullets always pass.
        let ok = &parse_issue_body("## Traces to\n\n- FR-X [criteria: a] -- prose\n").trace[0];
        assert!(trace_bullet_bracket_defect(ok).is_none());
        let bare = &parse_issue_body("## Traces to\n\n- FR-X -- prose\n").trace[0];
        assert!(trace_bullet_bracket_defect(bare).is_none());
        let none = &parse_issue_body("## Traces to\n\n- None -- reason\n").trace[0];
        assert!(trace_bullet_bracket_defect(none).is_none());
    }

    // -- #961: acceptance/done-when merge, not clobber ----------------------

    /// A body carrying both an `## Acceptance criteria` heading and a
    /// `## Done When` heading must keep both sets of criteria. Previously
    /// each acceptance-family heading ASSIGNED into `parsed.acceptance`, so
    /// whichever heading `extract_sections` visited last silently discarded
    /// the other's criteria -- exactly the shape #947's corrupted body
    /// carried after a lossy `legion issue view` -> `issue edit --body`
    /// round trip.
    #[test]
    fn acceptance_and_done_when_headings_merge_not_clobber() {
        let body = "## Acceptance criteria\n\n- [ ] First\n\n## Done When\n\n- [ ] Second\n";
        let parsed = parse_issue_body(body);
        assert_eq!(
            parsed.acceptance,
            vec!["First".to_owned(), "Second".to_owned()],
            "both headings' criteria must be present, got {:?}",
            parsed.acceptance
        );
    }

    // -- #961: `split_body_lossless` byte-exact round trip -------------------

    fn reassemble(preamble: &str, sections: &[(String, String)]) -> String {
        let mut out = preamble.to_string();
        for (heading, content) in sections {
            out.push_str("## ");
            out.push_str(heading);
            out.push_str(content);
        }
        out
    }

    #[test]
    fn split_body_lossless_round_trips_with_preamble() {
        let body = "Some intro text before any heading.\n\n\
                     ## Problem\n\nBroken.\n\n\
                     ## Acceptance criteria\n\n- [ ] Fix it\n";
        let (preamble, sections) = split_body_lossless(body);
        assert_eq!(preamble, "Some intro text before any heading.\n\n");
        assert_eq!(sections.len(), 2);
        assert_eq!(reassemble(&preamble, &sections), body);
    }

    #[test]
    fn split_body_lossless_round_trips_without_preamble() {
        let body = "## Problem\n\nBroken.\n\n## Acceptance criteria\n\n- [ ] Fix it\n";
        let (preamble, sections) = split_body_lossless(body);
        assert_eq!(preamble, "");
        assert_eq!(sections.len(), 2);
        assert_eq!(reassemble(&preamble, &sections), body);
    }

    #[test]
    fn split_body_lossless_round_trips_with_no_headings() {
        let body = "Just plain text, no headings at all.";
        let (preamble, sections) = split_body_lossless(body);
        assert_eq!(preamble, body);
        assert!(sections.is_empty());
        assert_eq!(reassemble(&preamble, &sections), body);
    }

    /// #961 review fix: the doc comment on `split_body_lossless` claims a
    /// final heading with no trailing newline reconstructs exactly, with
    /// `content == ""` rather than a fabricated `"\n"`. Nothing proved
    /// that -- the no-headings fixture above takes the early return and
    /// never reaches the branch that handles this.
    #[test]
    fn split_body_lossless_round_trips_final_heading_without_trailing_newline() {
        let body = "## Problem\n\nBroken.\n\n## Done";
        let (preamble, sections) = split_body_lossless(body);
        assert_eq!(preamble, "");
        assert_eq!(sections.len(), 2);
        assert_eq!(sections[1].0, "Done");
        assert_eq!(
            sections[1].1, "",
            "a final heading with no trailing newline must yield empty content, \
             not an invented newline"
        );
        assert_eq!(reassemble(&preamble, &sections), body);
    }

    #[test]
    fn split_body_lossless_preserves_checklist_and_fence_syntax() {
        // The whole point (#961): checkboxes, subheadings, and code fences
        // inside a section survive untouched -- unlike `extract_checklist`,
        // which rewrites `- [ ]`/`- [x]` to plain `- ` bullets.
        let body = "## Acceptance criteria\n\n- [ ] Tests pass\n- [x] Clippy clean\n\n### Notes\n\n```rust\nfn f() {}\n```\n";
        let (_preamble, sections) = split_body_lossless(body);
        assert_eq!(sections.len(), 1);
        let (heading, content) = &sections[0];
        assert_eq!(heading, "Acceptance criteria");
        assert!(content.contains("- [ ] Tests pass"));
        assert!(content.contains("- [x] Clippy clean"));
        assert!(content.contains("### Notes"));
        assert!(content.contains("```rust"));
    }

    #[test]
    fn trace_heading_tolerates_trailing_punctuation() {
        // Mirrors #907's acceptance-heading normalization test for the new
        // heading -- the same `key.trim_end_matches` call handles both.
        let body = "## Traces to:\n\n- FR-EMAIL-003 -- fixes it\n";
        let parsed = parse_issue_body(body);
        assert_eq!(parsed.trace.len(), 1);
    }
}
