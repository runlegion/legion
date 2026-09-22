//! The legion-cmd adapters: the I/O layer over the pure `legion_cmd::route`.
//!
//! `legion_cmd` decides; this module reads a hook payload, reads the policy
//! file, runs the lookups a matched rule requires, calls `route` once, and
//! turns the returned Decision into the shape the harness expects
//! (FR-CMD-017). No routing decision lives here (FR-CMD-011): nothing in this
//! module branches on a binary name or a verb.

pub(crate) mod config;
pub(crate) mod hook;
pub(crate) mod replacement;
