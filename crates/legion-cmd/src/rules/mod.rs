//! Non-Bash rule modules (FR-CMD-005 slice 3).
//!
//! Each submodule is a pure predicate over `(&ToolCall, &Ctx)` for the one
//! (or few) `Tool` variants its ported shell script matched -- no I/O, no
//! route table, no match arms shared with any other module. `no-gh` and the
//! twelve remaining scripts named in FR-CMD-005 each land here in their own
//! issue; this module only declares the ones that exist so far.
//!
//! Nothing wires these into `Router::route` yet -- that single-adapter
//! collapse is FR-CMD-007 (slice 9). Until then `plugin/hooks/*.sh` stays
//! live and authoritative (FR-CMD-016).

pub mod no_harness_explore;
