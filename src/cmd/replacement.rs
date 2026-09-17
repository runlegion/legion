//! Builds the replacement `tool_input` for a `Decision::Rewrite` (#1229,
//! FR-CMD-003). `route` names the managed target and returns the `Facts` it
//! extracted while deciding; it never builds the replacement command
//! itself, so this is the one place that turns those two things into an
//! actual command string -- without re-scanning the command `route` already
//! parsed.
//!
//! `route` guarantees `Decision::Rewrite` only for a single simple command
//! whose arguments equal a rule's declared `exact_args` exactly (reflection
//! 01a0ae1c), so this module's own job is narrower:
//!
//! 1. Substitutes `{repo}` in the target string (reflection 01a0ae13):
//!    legion's verbs require `--repo`, so a rewrite target that needs one
//!    is declared with a literal `{repo}` placeholder (e.g. `"legion issue
//!    list --repo {repo}"`) rather than a real name `route` has no way to
//!    know. `repo` is the caller's resolved repo name (from the hook
//!    payload's `cwd`, via `crate::cmd::hook::repo_from_cwd`). When the
//!    placeholder is present and the repo could not be resolved (the
//!    `"(unknown)"` sentinel `repo_from_cwd` returns), this FAILS CLOSED
//!    with `ReplacementError::UnknownRepo` rather than run a command
//!    against a bogus or literal `"(unknown)"` repo. A target with no
//!    `{repo}` placeholder is unaffected either way.
//! 2. Refuses (defense in depth) when `route`'s extracted `facts.paths` is
//!    non-empty. With `exact_args` enforced, a real `Decision::Rewrite`
//!    from `route` should never carry a path fact any more -- any
//!    invocation with an extra path-shaped argument now fails the
//!    `exact_args` match and denies before reaching a rewrite at all -- so
//!    this should be unreachable via `route` today. It stays as a backstop
//!    against a `Routed` value a caller builds directly (or a future
//!    policy shape this module has not seen) carrying facts inconsistent
//!    with a safe rewrite; `facts_from_scan` collects path-shaped operands
//!    across EVERY invocation in a compound command, not just the one
//!    whose rule matched, so appending one on trust risks attaching an
//!    unrelated invocation's operand to the target -- exactly the failure
//!    class `plugin/hooks/lib/emit.sh`'s `emit_rewrite` header documents
//!    paying for (#883: `git push && echo done` rewrote to `legion push
//!    --branch echo`).

use legion_cmd::{Facts, ManagedTarget};
use serde_json::Value;

/// The literal placeholder a rewrite target names when it needs the
/// caller's repo (reflection 01a0ae13): legion's verbs require `--repo`,
/// and `route` has no way to know the calling repo, so it declares the
/// need with this token instead. `pub(crate)` so `crate::cmd::hook`'s
/// display-only substitution (`substitute_repo_for_display`) uses the
/// same literal rather than a second hard-coded copy.
pub(crate) const REPO_PLACEHOLDER: &str = "{repo}";

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

    /// The target names `{repo}` but the calling repo could not be
    /// resolved from the hook payload's `cwd`. Never substitute the
    /// `"(unknown)"` sentinel into a runnable command -- that would run
    /// against a repo literally named "(unknown)", not merely an
    /// unhelpful one.
    #[error("rewrite target names {{repo}} but the calling repo could not be determined")]
    UnknownRepo,
}

/// Builds the replacement `tool_input` for a rewrite: the managed target's
/// command with `{repo}` substituted (see module doc), patched into
/// `original` so sibling Bash fields (`description`, `timeout`,
/// `run_in_background`) are preserved rather than dropped (the
/// whole-object `updatedInput` bug `plugin/hooks/lib/emit.sh`'s
/// `emit_rewrite` already paid for -- see its header comment). Refuses
/// when `facts.paths` is non-empty (see the module doc; should be
/// unreachable via `route` post-#1228) or when the target needs `{repo}`
/// and `repo` is the `"(unknown)"` sentinel.
///
/// `repo` is spliced directly into the returned command with no quoting
/// or escaping of its own -- this function trusts it completely. That
/// trust is only safe because `crate::cmd::hook::repo_from_cwd` validates
/// every repo name (`is_safe_repo_name`: ASCII letters, digits, `-`, `_`,
/// `.`, never starting with `.`) before returning anything other than the
/// `UNKNOWN_REPO` sentinel this function already refuses on. Never call
/// this with a `repo` from any other source.
pub(crate) fn build_replacement(
    target: &ManagedTarget,
    facts: &Facts,
    original: &Value,
    repo: &str,
) -> Result<Value, ReplacementError> {
    if target.as_str().is_empty() {
        return Err(ReplacementError::EmptyTarget);
    }
    if !facts.paths.is_empty() {
        return Err(ReplacementError::UnhandledPathFacts(facts.paths.len()));
    }

    let command = if target.as_str().contains(REPO_PLACEHOLDER) {
        if repo == crate::cmd::hook::UNKNOWN_REPO {
            return Err(ReplacementError::UnknownRepo);
        }
        target.as_str().replace(REPO_PLACEHOLDER, repo)
    } else {
        target.as_str().to_string()
    };

    let mut patched = match original {
        Value::Object(map) => Value::Object(map.clone()),
        _ => Value::Object(serde_json::Map::new()),
    };
    patched["command"] = Value::String(command);
    Ok(patched)
}

#[cfg(test)]
mod tests {
    use super::*;

    const REPO: &str = "legion";

    #[test]
    fn target_alone_replaces_command_with_no_facts() {
        let target = ManagedTarget::new("legion issue list");
        let original = serde_json::json!({"command": "gh issue list"});
        let replaced =
            build_replacement(&target, &Facts::default(), &original, REPO).expect("builds");
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
        let replaced =
            build_replacement(&target, &Facts::default(), &original, REPO).expect("builds");
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
        let err = build_replacement(&target, &facts, &original, REPO)
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
        let err = build_replacement(&target, &facts, &original, REPO)
            .expect_err("every path fact must be refused, not just the first");
        assert_eq!(err, ReplacementError::UnhandledPathFacts(2));
    }

    #[test]
    fn empty_target_is_rejected() {
        let target = ManagedTarget::new("");
        let original = serde_json::json!({"command": "gh issue list"});
        let err = build_replacement(&target, &Facts::default(), &original, REPO)
            .expect_err("empty target must not silently build a blank command");
        assert_eq!(err, ReplacementError::EmptyTarget);
    }

    #[test]
    fn a_non_object_original_input_still_produces_a_valid_replacement() {
        let target = ManagedTarget::new("legion issue list");
        let original = Value::Null;
        let replaced =
            build_replacement(&target, &Facts::default(), &original, REPO).expect("builds");
        assert_eq!(
            replaced,
            serde_json::json!({"command": "legion issue list"})
        );
    }

    #[test]
    fn a_repo_placeholder_is_substituted_with_the_resolved_repo() {
        let target = ManagedTarget::new("legion issue list --repo {repo}");
        let original = serde_json::json!({"command": "gh issue list"});
        let replaced =
            build_replacement(&target, &Facts::default(), &original, "my-repo").expect("builds");
        assert_eq!(
            replaced,
            serde_json::json!({"command": "legion issue list --repo my-repo"})
        );
    }

    #[test]
    fn a_repo_placeholder_with_an_unknown_repo_fails_closed() {
        let target = ManagedTarget::new("legion issue list --repo {repo}");
        let original = serde_json::json!({"command": "gh issue list"});
        let err = build_replacement(
            &target,
            &Facts::default(),
            &original,
            crate::cmd::hook::UNKNOWN_REPO,
        )
        .expect_err("an unresolved repo must never be substituted into a runnable command");
        assert_eq!(err, ReplacementError::UnknownRepo);
    }

    #[test]
    fn a_target_with_no_placeholder_is_unaffected_by_an_unknown_repo() {
        let target = ManagedTarget::new("legion issue list");
        let original = serde_json::json!({"command": "gh issue list"});
        let replaced = build_replacement(
            &target,
            &Facts::default(),
            &original,
            crate::cmd::hook::UNKNOWN_REPO,
        )
        .expect("a target with no placeholder never needs a repo");
        assert_eq!(
            replaced,
            serde_json::json!({"command": "legion issue list"})
        );
    }
}
