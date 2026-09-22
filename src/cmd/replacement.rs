//! Builds the replacement `tool_input` for a `Decision::Rewrite` (#1229,
//! FR-CMD-003).
//!
//! route names the managed target and returns the `Facts` it extracted while
//! deciding; it never emits the replacement command string. This module is
//! the one place that turns those two into a `tool_input`, and it never
//! scans the command string (FR-CMD-017).
//!
//! The replacement command is the target's text. A rewrite is only honest
//! when it is lossless (`plugin/hooks/lib/emit.sh`'s `emit_rewrite`: a
//! caller that would have to drop an operand the target cannot express must
//! deny instead, because the agent reads the result as the answer to its
//! original question). The target names no place for a path or an issue
//! number, so when route's facts carry either, the rewrite would drop an
//! operand the agent named; that is refused here and becomes a deny at the
//! adapter, never a silently narrower command.

use legion_cmd::{Facts, ManagedTarget};
use serde_json::{Map, Value};

/// Why a rewrite's replacement could not be built. Each becomes a deny at the
/// adapter (FR-CMD-009: no Decision drops the command without a message).
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub(crate) enum ReplacementError {
    /// The target names nothing to run. `parse_policy` rejects an empty
    /// rewrite target, so this is only reachable from a `Routed` built by
    /// hand; it stays a refusal rather than an empty command.
    #[error("the rewrite target is empty; nothing to run in place of the command")]
    EmptyTarget,

    /// The facts carry path operands the target has no place for.
    #[error("the rewrite would drop {0} path operand(s) the target does not carry")]
    PathOperandsNotCarried(usize),

    /// The facts carry issue numbers the target has no place for.
    #[error("the rewrite would drop {0} issue number(s) the target does not carry")]
    IssueNumbersNotCarried(usize),

    /// The original `tool_input` is an object with no `command` field (an
    /// Agent, Edit, or Write call). Inserting one would leave the fields the
    /// tool actually runs on untouched, and the adapter's `allow` would then
    /// grant the original call outright.
    #[error("the tool input has no command field to replace")]
    NoCommandField,
}

/// Builds the replacement `tool_input` for a rewrite: the target's command
/// patched into `original`, so the sibling Bash fields (`description`,
/// `timeout`, `run_in_background`) survive. `updatedInput` replaces the whole
/// `tool_input`, so rebuilding it from the command alone would turn a
/// background command into a foreground one and drop a raised timeout (the
/// bug `emit.sh`'s `emit_rewrite` already paid for). Refuses when the facts
/// carry an operand the target cannot express (see the module doc).
pub(crate) fn build_replacement(
    target: &ManagedTarget,
    facts: &Facts,
    original: &Value,
) -> Result<Value, ReplacementError> {
    if target.as_str().is_empty() {
        return Err(ReplacementError::EmptyTarget);
    }
    if !facts.paths.is_empty() {
        return Err(ReplacementError::PathOperandsNotCarried(facts.paths.len()));
    }
    if !facts.issue_numbers.is_empty() {
        return Err(ReplacementError::IssueNumbersNotCarried(
            facts.issue_numbers.len(),
        ));
    }

    let mut patched: Map<String, Value> = match original {
        Value::Object(map) if !map.contains_key("command") => {
            return Err(ReplacementError::NoCommandField);
        }
        Value::Object(map) => map.clone(),
        _ => Map::new(),
    };
    patched.insert(
        "command".to_string(),
        Value::String(target.as_str().to_string()),
    );
    Ok(Value::Object(patched))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target(name: &str) -> ManagedTarget {
        ManagedTarget::new(name)
    }

    #[test]
    fn the_target_replaces_the_command() {
        let original = serde_json::json!({"command": "gh issue list"});
        let replaced =
            build_replacement(&target("legion issue list"), &Facts::default(), &original)
                .expect("builds");
        assert_eq!(
            replaced,
            serde_json::json!({"command": "legion issue list"})
        );
    }

    #[test]
    fn sibling_bash_fields_are_preserved() {
        let original = serde_json::json!({
            "command": "gh issue list",
            "description": "list issues",
            "timeout": 5000,
            "run_in_background": true
        });
        let replaced =
            build_replacement(&target("legion issue list"), &Facts::default(), &original)
                .expect("builds");
        assert_eq!(
            replaced,
            serde_json::json!({
                "command": "legion issue list",
                "description": "list issues",
                "timeout": 5000,
                "run_in_background": true
            })
        );
    }

    #[test]
    fn keywords_and_a_verb_do_not_block_the_rewrite() {
        // The verb and the family's own subcommand words are facts too; they
        // are what the target replaces, not operands it drops.
        let facts = Facts {
            verb: Some("issue".to_string()),
            keywords: vec!["issue".to_string(), "list".to_string()],
            ..Facts::default()
        };
        let original = serde_json::json!({"command": "gh issue list"});
        assert!(build_replacement(&target("legion issue list"), &facts, &original).is_ok());
    }

    #[test]
    fn path_operands_are_refused_not_dropped() {
        let facts = Facts {
            paths: vec!["src/main.rs".to_string()],
            ..Facts::default()
        };
        let original = serde_json::json!({"command": "grep -rn foo src/main.rs"});
        let err = build_replacement(&target("legion sym etc find-content"), &facts, &original)
            .expect_err("a path the target cannot carry must refuse");
        assert_eq!(err, ReplacementError::PathOperandsNotCarried(1));
    }

    #[test]
    fn issue_numbers_are_refused_not_dropped() {
        let facts = Facts {
            issue_numbers: vec![123],
            ..Facts::default()
        };
        let original = serde_json::json!({"command": "gh issue view \"#123\""});
        let err = build_replacement(&target("legion issue view"), &facts, &original)
            .expect_err("an issue number the target cannot carry must refuse");
        assert_eq!(err, ReplacementError::IssueNumbersNotCarried(1));
    }

    #[test]
    fn an_empty_target_is_refused() {
        let original = serde_json::json!({"command": "gh issue list"});
        let err = build_replacement(&target(""), &Facts::default(), &original)
            .expect_err("an empty target must not become an empty command");
        assert_eq!(err, ReplacementError::EmptyTarget);
    }

    #[test]
    fn a_tool_input_without_a_command_is_refused() {
        let original = serde_json::json!({"subagent_type": "Explore", "prompt": "map it"});
        let err = build_replacement(
            &target("legion:legion-explore"),
            &Facts::default(),
            &original,
        )
        .expect_err("a rewrite must not invent a command field");
        assert_eq!(err, ReplacementError::NoCommandField);
    }

    #[test]
    fn a_non_object_original_still_yields_a_command() {
        let replaced = build_replacement(
            &target("legion issue list"),
            &Facts::default(),
            &Value::Null,
        )
        .expect("builds");
        assert_eq!(
            replaced,
            serde_json::json!({"command": "legion issue list"})
        );
    }
}
