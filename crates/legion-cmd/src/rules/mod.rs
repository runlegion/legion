//! The thirteen ported rule modules (FR-CMD-005, slice 3).
//!
//! Each non-Bash module here is code: path and content predicates over a
//! `ToolCall` and `Ctx` returning `Decision::Allow` or `Decision::Deny`.
//! Sibling modules land on their own branches; this file only adds its own
//! `pub mod` line to keep merges trivial.

pub mod pre_read_sym;
