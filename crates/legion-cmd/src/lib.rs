//! The legion-cmd Decision contract.
//!
//! This crate holds the types `route` will return once it is built
//! (policy-and-evaluator issue, tracked separately): the closed five-arm
//! `Decision` set, the closed seven-reason `ProxyReason` set, the facts
//! `route` extracts from a command, and the context a caller supplies to it.
//!
//! `route` itself is not implemented here. This crate has no filesystem,
//! network, database, or process dependency (NFR-CMD-001, FR-CMD-014): it is
//! pure data and pure functions over that data.

mod decision;
mod tokenizer;

pub use decision::{
    AskDetails, Context, ContractError, Decision, DenyDetails, Facts, Lookup, ManagedTarget,
    NO_GO_INSTEAD, ProxyReason, Routed, ToolCall,
};
pub use tokenizer::{Invocation, Opaque, Position, Scan, ScanError, scan};
