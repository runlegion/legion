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
    /// Other sections keyed by heading name
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub sections: Vec<(String, String)>,
    /// Raw body fallback when no headings found
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
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
        let key = heading.to_lowercase();
        let content = content.trim();
        if content.is_empty() {
            continue;
        }

        match key.as_str() {
            "problem" | "bug" | "issue" => {
                parsed.problem = Some(first_paragraph(content));
            }
            "solution" | "fix" | "what" | "proposal" => {
                parsed.solution = Some(first_paragraph(content));
            }
            "acceptance criteria" | "acceptance" | "done when" | "done" => {
                parsed.acceptance = extract_checklist(content);
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

/// Truncate a string to at most `max` characters, appending "..." if truncated.
/// Safe for multi-byte UTF-8. When `max < 3`, returns the leading `max`
/// characters with no ellipsis -- there isn't room to keep both content and
/// the marker, and the previous `max - 3` underflowed on `usize`. Callers
/// asking for a preview shorter than the ellipsis are getting a hard
/// truncation by design (#346).
pub fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    if max < 3 {
        return s.chars().take(max).collect();
    }
    let end: String = s.chars().take(max - 3).collect();
    format!("{end}...")
}

/// Format a card summary from stored structured fields.
/// Accepts the pre-parsed fields directly -- no reconstruction needed.
pub fn card_summary(
    problem: Option<&str>,
    acceptance: Option<&str>,
    context: Option<&str>,
) -> Option<String> {
    let mut parts: Vec<String> = Vec::new();

    if let Some(p) = problem {
        parts.push(truncate_chars(p, 120));
    }

    if let Some(acc) = acceptance {
        let count = acc.lines().count();
        if count > 0 {
            parts.push(format!("[{count} criteria]"));
        }
    }

    if parts.is_empty() {
        return context.map(|c| truncate_chars(c, 120));
    }

    Some(parts.join(" "))
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn card_summary_with_problem_and_criteria() {
        let summary = card_summary(Some("Things are broken"), Some("a\nb"), None).expect("summary");
        assert!(summary.contains("Things are broken"));
        assert!(summary.contains("[2 criteria]"));
    }

    #[test]
    fn card_summary_fallback_to_context() {
        let summary = card_summary(None, None, Some("Raw description")).expect("summary");
        assert_eq!(summary, "Raw description");
    }
}
