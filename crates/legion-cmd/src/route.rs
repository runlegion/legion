//! `route`: the one entry point an adapter calls (FR-CMD-011, FR-CMD-014).
//!
//! This module holds no routing logic of its own -- every branch that
//! decides a [`Decision`] lives in [`crate::evaluate`]. `route` exists so
//! callers have a stable, minimal signature to depend on: given a parsed
//! [`Policy`], a [`ToolCall`], and a [`Context`], it returns exactly one
//! [`Routed`] decision (FR-CMD-001). It performs no I/O of its own
//! (NFR-CMD-001) and delegates to no process or engine outside this crate
//! (FR-CMD-014): reading `policy.json` from disk is the adapter's job.

use crate::decision::{Context, Routed, ToolCall};
use crate::evaluate::evaluate;
use crate::policy::Policy;

/// The one routing entry point. Pure (NFR-CMD-001): given the same
/// `policy`, `call`, and `ctx`, `route` always returns the same `Routed`.
pub fn route(policy: &Policy, call: &ToolCall, ctx: &Context) -> Routed {
    evaluate(policy, call, ctx)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decision::{DecidingEntry, Decision};
    use crate::policy::parse_policy;

    fn bash_call(command: &str) -> ToolCall {
        ToolCall {
            tool: "Bash".to_string(),
            input: serde_json::json!({"command": command}),
        }
    }

    #[test]
    fn route_is_total_and_never_panics_on_empty_input() {
        let policy = parse_policy("{}").expect("empty policy parses");
        let routed = route(&policy, &bash_call(""), &Context::default());
        // Empty policy: every command denies (FR-CMD-016), including the
        // empty command.
        assert!(matches!(routed.decision, Decision::Deny(_)));
    }

    #[test]
    fn route_result_depends_only_on_its_inputs() {
        let policy = parse_policy(
            r#"{"tools": {"Bash": {"kind": "bash", "families": {"chmod": {"rules": [
                {"id": "r1", "predicate": {"operand_contains": "777"},
                 "outcome": {"kind": "deny", "reason": "no", "instead": "chmod 755"}}
            ]}}}}}"#,
        )
        .expect("valid policy");
        let ctx = Context::default();
        let call = bash_call("chmod 777 x");
        let first = route(&policy, &call, &ctx);
        let second = route(&policy, &call, &ctx);
        assert_eq!(first, second);
    }

    #[test]
    fn route_delegates_entirely_to_evaluate_and_names_the_matched_rule() {
        let policy = parse_policy(
            r#"{"tools": {"Bash": {"kind": "bash", "families": {"chmod": {"rules": [
                {"id": "chmod-777", "predicate": {"operand_contains": "777"},
                 "outcome": {"kind": "deny", "reason": "no", "instead": "chmod 755"}}
            ]}}}}}"#,
        )
        .expect("valid policy");
        let routed = route(&policy, &bash_call("chmod 777 x"), &Context::default());
        assert_eq!(
            routed.entry,
            DecidingEntry::Rule {
                id: "chmod-777".to_string(),
                needs_operator: false
            }
        );
    }
}
