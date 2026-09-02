//! Non-Bash rule modules (FR-CMD-005).
//!
//! A non-Bash module is plain code, not a route-table section: a path or
//! content predicate over one tool's input returning `Allow` or `Deny`, with
//! no `ArgKind` and no table row. Each lives in its own file, named for the
//! hook script it replaces.

pub mod no_local_memory;
