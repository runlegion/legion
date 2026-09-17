//! Builds the replacement `tool_input` for a `Decision::Rewrite` (#1229,
//! FR-CMD-003). `route` names the managed target and returns the `Facts` it
//! extracted while deciding; it never builds the replacement command
//! itself, so this is the one place that turns those two things into an
//! actual command string -- without re-scanning the command `route` already
//! parsed.
//!
//! The shipped policy's one rewrite rule (`gh issue list` -> `legion issue
//! list`) needs no arguments appended: the target string alone is already
//! the complete replacement. `facts.paths` is folded in here for a future
//! rewrite rule whose target needs the file operand carried forward (e.g.
//! rewriting a single-file command); no shipped rule exercises that path
//! today. `facts.verb`/`facts.issue_numbers`/`facts.keywords` are not
//! folded in -- the documents (FR-CMD-003) fix only that `route` returns
//! these facts and the adapter builds the command from them without
//! re-parsing, not a composition algorithm, so this is a deliberately
//! narrow starting point, not a claim that no other fact ever matters.

use legion_cmd::{Facts, ManagedTarget};
use serde_json::Value;

/// Errors building a `Decision::Rewrite`'s replacement command.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub(crate) enum ReplacementError {
    /// `route` named an empty managed target. `parse_policy` already
    /// rejects an empty `"target"` field in the shipped policy format, so
    /// this only fires on a `Routed` a caller built directly (as this
    /// issue's own ask/rewrite tests do) with an invalid `ManagedTarget`.
    #[error("rewrite target is empty; nothing to run in place of the command")]
    EmptyTarget,
}

/// Builds the replacement `tool_input` for a rewrite: the managed target's
/// command, with any path facts appended, patched into `original` so
/// sibling Bash fields (`description`, `timeout`, `run_in_background`) are
/// preserved rather than dropped (the whole-object `updatedInput` bug
/// `plugin/hooks/lib/emit.sh`'s `emit_rewrite` already paid for -- see its
/// header comment).
pub(crate) fn build_replacement(
    target: &ManagedTarget,
    facts: &Facts,
    original: &Value,
) -> Result<Value, ReplacementError> {
    if target.as_str().is_empty() {
        return Err(ReplacementError::EmptyTarget);
    }

    let mut command = target.as_str().to_string();
    for path in &facts.paths {
        command.push(' ');
        command.push_str(&shell_quote(path));
    }

    let mut patched = match original {
        Value::Object(map) => Value::Object(map.clone()),
        _ => Value::Object(serde_json::Map::new()),
    };
    patched["command"] = Value::String(command);
    Ok(patched)
}

/// Single-quotes `arg` for safe inclusion in a shell command line, escaping
/// any embedded single quote with the standard `'\''` sequence. Only used
/// for facts already extracted from a parsed command, never for raw
/// caller-supplied text.
fn shell_quote(arg: &str) -> String {
    format!("'{}'", arg.replace('\'', r"'\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn target_alone_replaces_command_with_no_facts() {
        let target = ManagedTarget::new("legion issue list");
        let original = serde_json::json!({"command": "gh issue list"});
        let replaced = build_replacement(&target, &Facts::default(), &original).expect("builds");
        assert_eq!(
            replaced,
            serde_json::json!({"command": "legion issue list"})
        );
    }

    #[test]
    fn sibling_bash_fields_are_preserved() {
        let target = ManagedTarget::new("legion issue list");
        let original = serde_json::json!({
            "command": "gh issue list",
            "description": "list issues",
            "timeout": 5000,
            "run_in_background": false
        });
        let replaced = build_replacement(&target, &Facts::default(), &original).expect("builds");
        assert_eq!(
            replaced,
            serde_json::json!({
                "command": "legion issue list",
                "description": "list issues",
                "timeout": 5000,
                "run_in_background": false
            })
        );
    }

    #[test]
    fn path_facts_are_appended_shell_quoted() {
        let target = ManagedTarget::new("legion sym def");
        let facts = Facts {
            paths: vec!["src/main.rs".to_string()],
            ..Facts::default()
        };
        let original = serde_json::json!({"command": "grep -rn foo src/main.rs"});
        let replaced = build_replacement(&target, &facts, &original).expect("builds");
        assert_eq!(
            replaced,
            serde_json::json!({"command": "legion sym def 'src/main.rs'"})
        );
    }

    #[test]
    fn a_path_containing_a_single_quote_is_escaped() {
        let target = ManagedTarget::new("legion sym def");
        let facts = Facts {
            paths: vec!["it's/mine.rs".to_string()],
            ..Facts::default()
        };
        let original = serde_json::json!({"command": "cat it's/mine.rs"});
        let replaced = build_replacement(&target, &facts, &original).expect("builds");
        assert_eq!(
            replaced,
            serde_json::json!({"command": r"legion sym def 'it'\''s/mine.rs'"})
        );
    }

    #[test]
    fn empty_target_is_rejected() {
        let target = ManagedTarget::new("");
        let original = serde_json::json!({"command": "gh issue list"});
        let err = build_replacement(&target, &Facts::default(), &original)
            .expect_err("empty target must not silently build a blank command");
        assert_eq!(err, ReplacementError::EmptyTarget);
    }

    #[test]
    fn a_non_object_original_input_still_produces_a_valid_replacement() {
        let target = ManagedTarget::new("legion issue list");
        let original = Value::Null;
        let replaced = build_replacement(&target, &Facts::default(), &original).expect("builds");
        assert_eq!(
            replaced,
            serde_json::json!({"command": "legion issue list"})
        );
    }
}
