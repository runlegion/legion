use std::collections::HashMap;

use crate::error;

/// Maximum length for a signal note (bytes). Signals are pings, not content delivery.
pub const MAX_SIGNAL_NOTE_LENGTH: usize = 280;

/// Validate that a signal note is within the length limit.
pub fn validate_note(note: &str) -> error::Result<()> {
    if note.len() > MAX_SIGNAL_NOTE_LENGTH {
        return Err(error::LegionError::SignalNoteTooLong {
            len: note.len(),
            max: MAX_SIGNAL_NOTE_LENGTH,
        });
    }
    Ok(())
}

/// A parsed signal from a bullpen post.
///
/// Signals follow the format: `@recipient verb:status {key: value, key: value}`
/// The details block is optional. Multi-word values are supported.
#[derive(Debug, Clone, PartialEq)]
pub struct Signal {
    pub recipient: String,
    pub verb: String,
    pub status: Option<String>,
    pub details: HashMap<String, String>,
    pub trailing: Option<String>,
}

/// Check if a bullpen post text is a signal (starts with @).
pub fn is_signal(text: &str) -> bool {
    text.trim_start().starts_with('@')
}

/// Parse a signal from bullpen post text.
///
/// Format: `@recipient verb:status {key: value, key: value}`
/// Also: `@recipient verb: free text after the verb`
///
/// Returns None if the text is not a valid signal.
pub fn parse_signal(text: &str) -> Option<Signal> {
    let trimmed = text.trim();
    if !trimmed.starts_with('@') {
        return None;
    }

    // Split off the @recipient
    let after_at = &trimmed[1..];
    let (recipient, rest) = split_first_word(after_at)?;

    // Parse verb:status or just verb:
    let (verb_part, after_verb) = if let Some(brace_pos) = rest.find('{') {
        let before_brace = rest[..brace_pos].trim();
        let after_brace = &rest[brace_pos..];
        (before_brace.to_string(), after_brace.to_string())
    } else {
        (rest.to_string(), String::new())
    };

    // Split verb:status
    let (verb, status, trailing_text) = parse_verb_part(&verb_part);

    if verb.is_empty() {
        return None;
    }

    // Parse {details} block if present
    let details = if after_verb.starts_with('{') {
        parse_details_block(&after_verb)
    } else {
        HashMap::new()
    };

    // Combine trailing text from after verb:status with any text after }
    let trailing_after_brace = if let Some(close) = after_verb.find('}') {
        let after_close = after_verb[close + 1..].trim();
        if after_close.is_empty() {
            None
        } else {
            Some(after_close.to_string())
        }
    } else {
        None
    };

    let trailing = match (trailing_text, trailing_after_brace) {
        (Some(t), Some(a)) => Some(format!("{} {}", t, a)),
        (Some(t), None) => Some(t),
        (None, Some(a)) => Some(a),
        (None, None) => None,
    };

    Some(Signal {
        recipient,
        verb,
        status,
        details,
        trailing,
    })
}

/// Format a signal for posting to the bullpen.
///
/// Constructs the `@recipient verb:status {details}` format.
pub fn format_signal(
    to: &str,
    verb: &str,
    status: Option<&str>,
    note: Option<&str>,
    details: &[(String, String)],
) -> String {
    let mut output = format!("@{} {}", to, verb);

    if let Some(s) = status {
        output.push(':');
        output.push_str(s);
    }

    if !details.is_empty() {
        let pairs: Vec<String> = details
            .iter()
            .map(|(k, v)| format!("{}: {}", k, v))
            .collect();
        output.push_str(&format!(" {{{}}}", pairs.join(", ")));
    }

    if let Some(n) = note {
        output.push_str(&format!(" -- {}", n));
    }

    output
}

/// Format a signal compactly for bullpen display (one-liner).
pub fn format_signal_compact(signal: &Signal, repo: &str, date: &str) -> String {
    let status_part = signal
        .status
        .as_deref()
        .map(|s| format!(":{}", s))
        .unwrap_or_default();

    let details_part = if signal.details.is_empty() {
        String::new()
    } else {
        let pairs: Vec<String> = signal
            .details
            .iter()
            .map(|(k, v)| format!("{}: {}", k, v))
            .collect();
        format!(" {{{}}}", pairs.join(", "))
    };

    let trailing_part = signal
        .trailing
        .as_deref()
        .map(|t| {
            let clean = t.strip_prefix("-- ").unwrap_or(t);
            format!(" -- {}", clean)
        })
        .unwrap_or_default();

    format!(
        "[{}] @{} {}{}{}{}  ({})",
        repo, signal.recipient, signal.verb, status_part, details_part, trailing_part, date
    )
}

/// Split the first whitespace-delimited word from a string.
fn split_first_word(s: &str) -> Option<(String, String)> {
    let trimmed = s.trim();
    if trimmed.is_empty() {
        return None;
    }
    match trimmed.find(char::is_whitespace) {
        Some(pos) => Some((
            trimmed[..pos].to_string(),
            trimmed[pos..].trim().to_string(),
        )),
        None => Some((trimmed.to_string(), String::new())),
    }
}

/// Parse `verb:status trailing text` into (verb, Option<status>, Option<trailing>).
fn parse_verb_part(s: &str) -> (String, Option<String>, Option<String>) {
    let trimmed = s.trim();
    if trimmed.is_empty() {
        return (String::new(), None, None);
    }

    // Split on first whitespace to get verb:status part and trailing
    let (first_token, trailing) = match trimmed.find(char::is_whitespace) {
        Some(pos) => (trimmed[..pos].to_string(), {
            let t = trimmed[pos..].trim();
            if t.is_empty() {
                None
            } else {
                Some(t.to_string())
            }
        }),
        None => (trimmed.to_string(), None),
    };

    // Split verb:status on first colon
    match first_token.find(':') {
        Some(pos) => {
            let verb = first_token[..pos].to_string();
            let status = first_token[pos + 1..].to_string();
            let status = if status.is_empty() {
                None
            } else {
                Some(status)
            };
            (verb, status, trailing)
        }
        None => (first_token, None, trailing),
    }
}

/// Parse a `{key: value, key: value}` details block.
fn parse_details_block(s: &str) -> HashMap<String, String> {
    let mut result = HashMap::new();
    let trimmed = s.trim();

    // Find content between { and }
    let start = match trimmed.find('{') {
        Some(pos) => pos + 1,
        None => return result,
    };
    let end = match trimmed.find('}') {
        Some(pos) => pos,
        None => trimmed.len(),
    };

    let content = &trimmed[start..end];

    // Split on commas, then parse key: value
    for pair in content.split(',') {
        let pair = pair.trim();
        if let Some(colon_pos) = pair.find(':') {
            let key = pair[..colon_pos].trim().to_string();
            let value = pair[colon_pos + 1..].trim().to_string();
            if !key.is_empty() {
                result.insert(key, value);
            }
        }
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_signal_detects_at_prefix() {
        assert!(is_signal("@legion review:approved"));
        assert!(is_signal("  @all announce: shipped"));
        assert!(!is_signal("just a regular post"));
        assert!(!is_signal("email@example.com is not a signal"));
    }

    #[test]
    fn parse_simple_signal() {
        let signal = parse_signal("@legion review:approved").unwrap();
        assert_eq!(signal.recipient, "legion");
        assert_eq!(signal.verb, "review");
        assert_eq!(signal.status.as_deref(), Some("approved"));
        assert!(signal.details.is_empty());
    }

    #[test]
    fn parse_signal_with_details() {
        let signal =
            parse_signal("@legion review:approved {surface: cap-output, chain: confirmed}")
                .unwrap();
        assert_eq!(signal.recipient, "legion");
        assert_eq!(signal.verb, "review");
        assert_eq!(signal.status.as_deref(), Some("approved"));
        assert_eq!(
            signal.details.get("surface").map(|s| s.as_str()),
            Some("cap-output")
        );
        assert_eq!(
            signal.details.get("chain").map(|s| s.as_str()),
            Some("confirmed")
        );
    }

    #[test]
    fn parse_signal_verb_only() {
        let signal = parse_signal("@all announce: PR #85 merged").unwrap();
        assert_eq!(signal.recipient, "all");
        assert_eq!(signal.verb, "announce");
        assert!(signal.status.is_none());
        assert_eq!(signal.trailing.as_deref(), Some("PR #85 merged"));
    }

    #[test]
    fn parse_signal_with_trailing_text() {
        let signal = parse_signal("@kelex question: does --follows work cross-agent?").unwrap();
        assert_eq!(signal.recipient, "kelex");
        assert_eq!(signal.verb, "question");
        assert!(signal.status.is_none());
        assert_eq!(
            signal.trailing.as_deref(),
            Some("does --follows work cross-agent?")
        );
    }

    #[test]
    fn parse_non_signal_returns_none() {
        assert!(parse_signal("just a post").is_none());
        assert!(parse_signal("").is_none());
    }

    #[test]
    fn parse_signal_recipient_only_returns_none() {
        // @recipient with no verb is not a valid signal
        assert!(parse_signal("@legion").is_none());
    }

    #[test]
    fn format_signal_basic() {
        let result = format_signal("legion", "review", Some("approved"), None, &[]);
        assert_eq!(result, "@legion review:approved");
    }

    #[test]
    fn format_signal_with_details() {
        let details = vec![("surface".to_string(), "cap-output".to_string())];
        let result = format_signal("legion", "review", Some("approved"), None, &details);
        assert_eq!(result, "@legion review:approved {surface: cap-output}");
    }

    #[test]
    fn format_signal_with_note() {
        let result = format_signal("all", "announce", None, Some("PR #85 merged"), &[]);
        assert_eq!(result, "@all announce -- PR #85 merged");
    }

    #[test]
    fn format_signal_compact_display() {
        let signal = Signal {
            recipient: "legion".into(),
            verb: "review".into(),
            status: Some("approved".into()),
            details: HashMap::new(),
            trailing: None,
        };
        let output = format_signal_compact(&signal, "kelex", "2026-03-09");
        assert_eq!(output, "[kelex] @legion review:approved  (2026-03-09)");
    }

    #[test]
    fn roundtrip_format_parse() {
        let formatted = format_signal(
            "platform",
            "request",
            Some("help"),
            Some("need Rust expertise"),
            &[],
        );
        let parsed = parse_signal(&formatted).unwrap();
        assert_eq!(parsed.recipient, "platform");
        assert_eq!(parsed.verb, "request");
        assert_eq!(parsed.status.as_deref(), Some("help"));
    }

    #[test]
    fn validate_note_rejects_long_notes() {
        let long = "a".repeat(MAX_SIGNAL_NOTE_LENGTH + 1);
        let result = validate_note(&long);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("legion post"));
    }

    #[test]
    fn validate_note_accepts_short_notes() {
        assert!(validate_note("short note").is_ok());
        assert!(validate_note(&"a".repeat(MAX_SIGNAL_NOTE_LENGTH)).is_ok());
    }

    #[test]
    fn compact_display_with_note_no_double_dashes() {
        let formatted = format_signal("all", "announce", None, Some("Phase 2.1 shipped"), &[]);
        let parsed = parse_signal(&formatted).unwrap();
        let compact = format_signal_compact(&parsed, "legion", "2026-03-09");
        assert!(
            !compact.contains("-- --"),
            "should not have double dashes: {compact}"
        );
        assert!(
            compact.contains("Phase 2.1 shipped"),
            "should contain note: {compact}"
        );
    }
}
