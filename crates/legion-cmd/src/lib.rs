//! One router, not a dozen guards.
//!
//! legion's command enforcement was roughly a dozen bash hooks, one per
//! managed binary, each matching on the leading token of a command and
//! proving argument-level losslessness in shell. That is structurally
//! incomplete for two reasons legion's own memory already named: a guard that
//! classifies by the first token cannot see past a pipe, an env prefix, or a
//! wrapper; and whether a rewrite is lossless is a property of the ARGUMENT,
//! not of the verb.
//!
//! This crate is the replacement: one pure function from a tool call and a
//! context to a decision.
//!
//! ```text
//! route(ToolCall, Ctx) -> Routed
//! ```
//!
//! PURE means what it says (FR-CMD-001): no process spawning, no database, no
//! shell-out, no environment read on the call path. The caller gathers what
//! the router needs into [`Ctx`] and does every side effect itself. That is
//! what lets the whole policy be unit-tested without a fixture repo.
//!
//! # What slice 1 contains
//!
//! The type shapes and the workspace carve, and nothing that decides. The
//! embedded [`RouteTable`] has every section present and every list empty, so
//! [`Router::route`] falls through to the table's `defaults.no_match` for
//! every call. The tokenizer (slice 2), the thirteen ported rule modules
//! (slice 3), the `cmd-check` verb (slice 4), the command record (slice 5),
//! predictions (slice 6), rulings (slice 7), pre-load recall (slice 8), and
//! the single PreToolUse adapter (slice 9) each land in their own issue.

#![forbid(unsafe_code)]

pub mod call;
pub mod ctx;
pub mod decision;
pub mod router;
pub mod table;

pub use call::{ParseError, Tool, ToolCall};
pub use ctx::{Ctx, RecallHit, Ruling};
pub use decision::{Carry, Decision, Matched, Routed, Targets};
pub use router::Router;
pub use table::{
    ArgKind, ArgOutcome, ArgPattern, DefaultOutcome, Defaults, Escape, FlagPolicy, FlagSpec,
    GroupHelp, Route, RouteTable, TableError, Wrapper,
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_crate_composes_end_to_end_from_a_harness_payload() {
        let payload = serde_json::json!({
            "tool_name": "Bash",
            "tool_input": { "command": "gh issue list" }
        });
        let call = ToolCall::from_hook_json(&payload).expect("parse");
        assert_eq!(call.tool(), Tool::Bash);

        let router = Router::new(RouteTable::embedded().expect("table")).expect("compile");
        let routed = router.route(&call, &Ctx::default());
        assert_eq!(routed.decision, Decision::Allow);
    }
}
