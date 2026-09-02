//! Redirect a built-in `Explore` subagent spawn to `legion:legion-explore`
//! (FR-CMD-005, ported from `plugin/hooks/no-harness-explore.sh`).
//!
//! The built-in `Explore` subagent greps and reads raw files. On a
//! legion-covered repo that is the wrong instrument: `legion:legion-explore`
//! orients through SCIP sym queries (def/refs/impl/hover) and recall/consult,
//! and returns conclusions with file:line citations instead of file dumps.
//! Swapping `subagent_type` is a single field substitution on the same
//! `Agent`/`Task` call, so the shell script rewrites the spawn rather than
//! refusing it -- it always emits `permissionDecision: allow` with
//! `updatedInput`, never a deny (`test-no-harness-explore.sh`, "the rewrite
//! is an allow with permissionDecision=allow, not a deny"). This module
//! reproduces that: it never returns `Decision::Deny`.
//!
//! `Decision::Rewrite`'s `command`/`substitutes` shape (FR-CMD-005 rev 8) is
//! what makes this a lossless port. `command` carries the replacement
//! `subagent_type` value and `substitutes` names the field it replaces;
//! `prompt` and every other field on the original `tool_input` (description,
//! isolation, model, name, ...) is left for the caller who applies this
//! decision to copy through untouched, exactly as the shell script's
//! `UPDATED_INPUT` construction does (`.tool_input | .subagent_type =
//! $target`). This module never reads `prompt` at all, so there is nothing
//! in it that could alter or drop it.

use crate::call::ToolCall;
use crate::ctx::Ctx;
use crate::decision::Decision;

/// The plugin agent `Explore` spawns are redirected to.
const TARGET: &str = "legion:legion-explore";

/// Decide an `Agent` or `Task` call requesting the built-in `Explore`
/// subagent.
///
/// A case-insensitive EXACT match on the whole `subagent_type` value against
/// `"explore"` returns a field-substitution `Decision::Rewrite` naming
/// `TARGET`. Anything else -- another agent name, a value that merely
/// contains "explore" (`code-explorer`), the redirect target itself
/// (`legion-explore`, `legion:legion-explore`), or a missing/empty value --
/// returns `Decision::Allow`. `ctx` is unused: this module's decision does
/// not depend on any `Ctx` field (FR-CMD-001; a future revision may change
/// that, which is why the parameter is threaded through rather than
/// dropped).
pub fn decide(call: &ToolCall, _ctx: &Ctx) -> Decision {
    let subagent_type = match call {
        ToolCall::Agent { subagent_type, .. } | ToolCall::Task { subagent_type, .. } => {
            subagent_type
        }
        _ => return Decision::Allow,
    };

    if !subagent_type.eq_ignore_ascii_case("explore") {
        return Decision::Allow;
    }

    Decision::Rewrite {
        command: TARGET.into(),
        reason: format!(
            "legion rewrote this spawn's subagent_type to {TARGET} -- it orients through SCIP \
             sym queries (def/refs/impl/hover) and recall/consult, returning conclusions with \
             file:line citations, where the built-in Explore greps and reads raw files"
        ),
        carry: vec![],
        substitutes: Some("subagent_type".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn agent(subagent_type: &str) -> ToolCall {
        ToolCall::Agent {
            prompt: "map the wake FSM".into(),
            subagent_type: subagent_type.into(),
        }
    }

    fn task(subagent_type: &str) -> ToolCall {
        ToolCall::Task {
            prompt: "map the wake FSM".into(),
            subagent_type: subagent_type.into(),
        }
    }

    fn assert_rewrites(subagent_type: &str) {
        for call in [agent(subagent_type), task(subagent_type)] {
            let decision = decide(&call, &Ctx::default());
            match decision {
                Decision::Rewrite {
                    command,
                    substitutes,
                    ..
                } => {
                    assert_eq!(command, TARGET, "{call:?} rewrites to the redirect target");
                    assert_eq!(
                        substitutes.as_deref(),
                        Some("subagent_type"),
                        "{call:?} names subagent_type as the substituted field"
                    );
                }
                other => panic!("expected a Rewrite for {call:?}, got {other:?}"),
            }
        }
    }

    fn assert_allows(subagent_type: &str) {
        for call in [agent(subagent_type), task(subagent_type)] {
            assert_eq!(
                decide(&call, &Ctx::default()),
                Decision::Allow,
                "{call:?} must be Allow"
            );
        }
    }

    #[test]
    fn an_exact_case_insensitive_match_on_explore_rewrites() {
        assert_rewrites("Explore");
        assert_rewrites("explore");
        assert_rewrites("EXPLORE");
    }

    #[test]
    fn the_rewrite_is_never_a_deny() {
        // Mirrors test-no-harness-explore.sh's "the rewrite is an allow with
        // permissionDecision=allow, not a deny" -- this module has no
        // reachable Deny arm at all.
        for subagent_type in ["Explore", "explore", "EXPLORE", "Plan", "", "code-explorer"] {
            for call in [agent(subagent_type), task(subagent_type)] {
                let decision = decide(&call, &Ctx::default());
                assert!(
                    !matches!(decision, Decision::Deny(_)),
                    "{call:?} must never deny, got {decision:?}"
                );
            }
        }
    }

    #[test]
    fn the_reason_names_the_redirect_target() {
        match decide(&agent("Explore"), &Ctx::default()) {
            Decision::Rewrite { reason, .. } => {
                assert!(reason.contains(TARGET), "reason names the target: {reason}");
            }
            other => panic!("expected Rewrite, got {other:?}"),
        }
    }

    #[test]
    fn prompt_is_never_inspected_and_does_not_change_the_decision() {
        let a = ToolCall::Agent {
            prompt: "one prompt".into(),
            subagent_type: "Explore".into(),
        };
        let b = ToolCall::Agent {
            prompt: "an entirely different prompt".into(),
            subagent_type: "Explore".into(),
        };
        assert_eq!(decide(&a, &Ctx::default()), decide(&b, &Ctx::default()));
    }

    #[test]
    fn the_redirect_target_itself_passes_through() {
        assert_allows("legion-explore");
        assert_allows("legion:legion-explore");
    }

    #[test]
    fn other_named_agents_pass_through() {
        assert_allows("Plan");
        assert_allows("general-purpose");
    }

    #[test]
    fn a_value_that_only_contains_explore_is_not_a_substring_match() {
        assert_allows("code-explorer");
    }

    #[test]
    fn a_missing_or_empty_subagent_type_allows() {
        assert_allows("");
    }

    #[test]
    fn a_call_this_module_does_not_cover_allows() {
        let call = ToolCall::Bash {
            command: "explore".into(),
        };
        assert_eq!(decide(&call, &Ctx::default()), Decision::Allow);
    }
}
