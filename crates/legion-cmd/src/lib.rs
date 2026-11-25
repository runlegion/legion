//! legion-cmd: route a tool call to one Decision from a declarative policy.
//!
//! [`route`] is the entry point. A Bash command is parsed once with the
//! [`scan`] splitter and put through the policy's four lists (#1337): a
//! never-run command is denied, an ask or power-switch command is asked
//! about, each proxied name gets `legion ` inserted before it
//! ([`proxy_insertion`]), and everything else runs as typed. A call to any
//! other tool is decided by its kind's rules. The policy is data:
//! [`parse_policy`] reads it, and no routing decision turns on a binary name
//! in Rust code (FR-CMD-011).
//!
//! This crate has no filesystem, network, database, or process dependency
//! (NFR-CMD-001, FR-CMD-014): it is pure data and pure functions over that
//! data.

mod decision;
mod evaluate;
mod insert;
mod lookups;
mod nogo;
mod policy;
mod route;
mod splitter;

pub use decision::{
    AskDetails, Context, ContractError, Deciding, Decision, DenyDetails, Facts, Lookup,
    ManagedTarget, NO_GO_INSTEAD, Routed, ToolCall,
};
pub use insert::{InsertError, Placement, insert_at, plan, proxy_insertion, verify};
pub use lookups::{RequiredLookups, required_lookups};
pub use nogo::{
    BUILTIN_DENY_PATTERNS, CommandKey, NoGoEntry, NoGoPredicate, builtin_no_go, command_key,
};
pub use policy::{Policy, PolicyError, Predicate, Rule, RuleOutcome, ToolKind, parse_policy};
pub use route::{PROXY_ID, route};
pub use splitter::{
    Invocation, MAX_DEPTH, Position, Scan, ScanError, Unreduced, UnreducedReason, scan, scan_at,
};
