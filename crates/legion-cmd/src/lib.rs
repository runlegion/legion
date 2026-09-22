//! legion-cmd: route a tool call to one Decision from a declarative policy.
//!
//! [`route`] is the entry point: it splits a Bash command with the [`scan`]
//! splitter, re-enters the wrapper and shell-interpreter payloads the policy
//! names, evaluates every invocation and unreduced region through the one
//! evaluator, and folds them into a single [`Decision`] (the closed five-arm
//! set) plus the [`Facts`] it extracted. The policy is data: [`parse_policy`]
//! reads it, and no routing decision turns on a binary name in Rust code
//! (FR-CMD-011).
//!
//! This crate has no filesystem, network, database, or process dependency
//! (NFR-CMD-001, FR-CMD-014): it is pure data and pure functions over that
//! data.

mod decision;
mod evaluate;
mod lookups;
mod policy;
mod route;
mod splitter;

pub use decision::{
    AskDetails, Context, ContractError, Deciding, Decision, DenyDetails, Facts, Lookup,
    ManagedTarget, NO_GO_INSTEAD, ProxyReason, Routed, ToolCall,
};
pub use lookups::{RequiredLookups, required_lookups};
pub use policy::{
    BodyLanguage, Family, Interpreter, Policy, PolicyError, Predicate, Rule, RuleOutcome,
    ScriptCarrier, SymJob, ToolKind, ToolRules, Wrapper, parse_policy,
};
pub use route::route;
pub use splitter::{
    Invocation, MAX_DEPTH, Position, Scan, ScanError, Unreduced, UnreducedReason, scan, scan_at,
};
