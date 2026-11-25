//! Builds the replacement `tool_input` for a changed call (#1229, FR-CMD-003,
//! #1337).
//!
//! route decides what replaces the call; this module is the one place that
//! patches it into a `tool_input`, and it never scans the command string
//! (FR-CMD-017). Which field changes depends on the tool: a Bash call's
//! `command` becomes the command route rewrote (`Facts::rewritten`, the
//! typed command with `legion ` before each proxied name), and an Agent or
//! Task spawn's `subagent_type` becomes the rule's target (the Explore
//! rewrite `plugin/hooks/no-harness-explore.sh` performed).
//! [`rewritable_field`] is the one place that choice is made.

use serde_json::{Map, Value};

/// Why a replacement could not be built. Each becomes a deny at the adapter
/// (FR-CMD-009: no Decision drops the command without a message).
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub(crate) enum ReplacementError {
    /// The value to put in place is empty. route never produces one, so this
    /// is only reachable from a `Routed` built by hand; it stays a refusal
    /// rather than an empty call.
    #[error("the replacement is empty; nothing to run in place of the call")]
    EmptyReplacement,

    /// The original `tool_input` is not an object carrying a field a rewrite
    /// can replace (an Edit or Write call, or a malformed input).
    #[error("the tool input has no command or subagent_type field to replace")]
    NoRewritableField,
}

/// The `tool_input` field a rewrite replaces: `command` for a Bash call,
/// `subagent_type` for an Agent or Task spawn. `None` for any other shape,
/// which the rewrite refuses rather than inventing a field.
pub(crate) fn rewritable_field(original: &Value) -> Option<&'static str> {
    let Value::Object(map) = original else {
        return None;
    };
    ["command", "subagent_type"]
        .into_iter()
        .find(|field| map.contains_key(*field))
}

/// `original` with its [`rewritable_field`] set to `value`, so every sibling
/// field survives -- a Bash call's `description`, `timeout`,
/// `run_in_background`; a spawn's `prompt` and `description`.
/// `updatedInput` replaces the whole `tool_input`, so rebuilding it from the
/// one field alone would turn a background command into a foreground one
/// and drop a raised timeout.
pub(crate) fn build_replacement(original: &Value, value: &str) -> Result<Value, ReplacementError> {
    if value.trim().is_empty() {
        return Err(ReplacementError::EmptyReplacement);
    }
    let (Some(field), Value::Object(map)) = (rewritable_field(original), original) else {
        return Err(ReplacementError::NoRewritableField);
    };
    let mut patched: Map<String, Value> = map.clone();
    patched.insert(field.to_string(), Value::String(value.to_string()));
    Ok(Value::Object(patched))
}

/// Single-quotes `text` for a shell command line, closing and reopening the
/// quote around every embedded single quote (`'\''`), so a command carrying
/// quotes or metacharacters stays one word.
pub(crate) fn shell_single_quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', r"'\''"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_bash_replacement_keeps_every_sibling_field() {
        let original = json!({
            "command": "git status",
            "description": "status",
            "timeout": 600000,
            "run_in_background": true
        });
        let patched = build_replacement(&original, "legion git status").expect("patched");
        assert_eq!(
            patched,
            json!({
                "command": "legion git status",
                "description": "status",
                "timeout": 600000,
                "run_in_background": true
            })
        );
    }

    #[test]
    fn a_spawn_replacement_patches_subagent_type() {
        let original = json!({"subagent_type": "Explore", "prompt": "map it"});
        let patched = build_replacement(&original, "legion:legion-explore").expect("patched");
        assert_eq!(
            patched,
            json!({"subagent_type": "legion:legion-explore", "prompt": "map it"})
        );
    }

    #[test]
    fn a_call_with_no_rewritable_field_or_an_empty_value_is_refused() {
        assert_eq!(
            build_replacement(&json!({"file_path": "a.rs"}), "x"),
            Err(ReplacementError::NoRewritableField)
        );
        assert_eq!(
            build_replacement(&json!({"command": "git status"}), " "),
            Err(ReplacementError::EmptyReplacement)
        );
        assert_eq!(rewritable_field(&json!("not an object")), None);
    }

    #[test]
    fn single_quoting_survives_embedded_quotes() {
        assert_eq!(shell_single_quote("it's"), r"'it'\''s'");
        assert_eq!(shell_single_quote("a b"), "'a b'");
    }
}
