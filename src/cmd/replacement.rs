//! Builds the replacement `tool_input` for a `Decision::Rewrite` (#1229,
//! FR-CMD-003). `route` names the managed target and returns the `Facts` it
//! extracted while deciding; it never builds the replacement command
//! itself, so this is the one place that turns those two things into an
//! actual command string -- without re-scanning the command `route` already
//! parsed.
//!
//! The shipped policy's one rewrite rule (`gh issue list` -> `legion issue
//! list`) needs no arguments appended: the target string alone is already
//! the complete replacement, and `facts.paths` is empty for it. `facts` is
//! still taken and checked here -- not merely accepted and ignored -- for
//! `facts.paths`: `facts_from_scan` collects path-shaped operands across
//! EVERY invocation in a compound command, not just the one whose rule
//! matched (see `crate::evaluate::facts_from_scan` upstream, and
//! `fold`'s strictest-decision selection), so blindly appending them would
//! silently attach an unrelated invocation's operand to the rewrite
//! target -- exactly the failure class
//! `plugin/hooks/lib/emit.sh`'s `emit_rewrite` header documents paying for
//! (#883: `git push && echo done` rewrote to `legion push --branch echo`).
//! `route` names a target with no notion of "the operand this rule's own
//! invocation carried" versus "an operand some other part of the command
//! carried", so the only lossless choice here is to refuse rather than
//! guess: a rewrite whose facts carry any path is denied, not silently
//! composed. Lossless composition of a matched invocation's own operands
//! into its rewrite target is #1228's lane, not this one's.

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

    /// `route`'s extracted facts carry one or more path operands. Nothing
    /// here can tell which invocation in a (possibly compound) command
    /// that path belongs to, so appending it risks attaching an unrelated
    /// invocation's operand to the rewrite target -- refusing is the only
    /// choice that cannot silently run something other than what was
    /// rewritten.
    #[error("rewrite facts carry {0} path operand(s), which this rewrite cannot safely compose")]
    UnhandledPathFacts(usize),
}

/// Builds the replacement `tool_input` for a rewrite: the managed target's
/// command, patched into `original` so sibling Bash fields (`description`,
/// `timeout`, `run_in_background`) are preserved rather than dropped (the
/// whole-object `updatedInput` bug `plugin/hooks/lib/emit.sh`'s
/// `emit_rewrite` already paid for -- see its header comment). Refuses
/// when `facts.paths` is non-empty (see the module doc); the shipped
/// policy's one rewrite rule never carries any.
pub(crate) fn build_replacement(
    target: &ManagedTarget,
    facts: &Facts,
    original: &Value,
) -> Result<Value, ReplacementError> {
    if target.as_str().is_empty() {
        return Err(ReplacementError::EmptyTarget);
    }
    if !facts.paths.is_empty() {
        return Err(ReplacementError::UnhandledPathFacts(facts.paths.len()));
    }

    let mut patched = match original {
        Value::Object(map) => Value::Object(map.clone()),
        _ => Value::Object(serde_json::Map::new()),
    };
    patched["command"] = Value::String(target.as_str().to_string());
    Ok(patched)
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
    fn path_facts_are_refused_not_silently_appended() {
        // `facts.paths` can carry an operand from a DIFFERENT invocation
        // in a compound command than the one the matched rule governs
        // (facts_from_scan collects across the whole scan); appending it
        // here would risk attaching an unrelated path to the rewrite
        // target, so this must refuse rather than compose (see module
        // doc, and #883 in `emit.sh`'s `emit_rewrite` header for the bug
        // class this avoids).
        let target = ManagedTarget::new("legion sym def");
        let facts = Facts {
            paths: vec!["src/main.rs".to_string()],
            ..Facts::default()
        };
        let original = serde_json::json!({"command": "grep -rn foo src/main.rs"});
        let err = build_replacement(&target, &facts, &original)
            .expect_err("a rewrite with path facts must refuse, not guess composition");
        assert_eq!(err, ReplacementError::UnhandledPathFacts(1));
    }

    #[test]
    fn multiple_path_facts_are_also_refused() {
        let target = ManagedTarget::new("legion sym def");
        let facts = Facts {
            paths: vec!["a.rs".to_string(), "b.rs".to_string()],
            ..Facts::default()
        };
        let original = serde_json::json!({"command": "grep -rn foo a.rs b.rs"});
        let err = build_replacement(&target, &facts, &original)
            .expect_err("every path fact must be refused, not just the first");
        assert_eq!(err, ReplacementError::UnhandledPathFacts(2));
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
