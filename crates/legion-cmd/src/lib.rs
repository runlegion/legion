//! The legion-cmd Decision contract, policy, and evaluator.
//!
//! This crate holds the closed five-arm `Decision` set, the closed
//! seven-reason `ProxyReason` set, the facts `route` extracts from a
//! command, the context a caller supplies to it, the declarative `Policy`
//! a single evaluator reads (#1227), and `route` itself: the one entry
//! point that turns a `ToolCall` and a `Context` into a `Routed` decision.
//!
//! This crate has no filesystem, network, database, or process dependency
//! (NFR-CMD-001, FR-CMD-014): it is pure data and pure functions over that
//! data. Reading the policy file from disk is the adapter's job, not
//! this crate's.

mod decision;
mod evaluate;
mod policy;
mod route;
mod tokenizer;

pub use decision::{
    AskDetails, Context, ContractError, DecidingEntry, Decision, DenyDetails, Facts, Lookup,
    ManagedTarget, NO_GO_INSTEAD, ProxyReason, Routed, ToolCall,
};
pub use policy::{
    BinaryOptions, Family, MatchInput, Policy, PolicyError, Predicate, RequiredLookup, Rule,
    SymJob, SymJobMatcher, ToolKind, ToolRules, parse_policy,
};
pub use route::route;
pub use tokenizer::{Invocation, MAX_DEPTH, Opaque, Position, Scan, ScanError, scan};
