//! Rule modules (FR-CMD-005).
//!
//! A non-Bash module is plain code: a path or content predicate over one
//! tool's input returning `Allow` or `Deny`, with no `ArgKind` and no table
//! row. A Bash module (`no_gh` and its siblings) is the opposite shape: a
//! route-table section in `route-table.toml` plus the tests ported from its
//! shell script, holding no match arms of its own -- its file here carries
//! only those tests, exercised through the one evaluator in `router.rs`.
//! Each module lives in its own file, named for the hook script it
//! replaces.
//!
//! Sibling modules land on their own branches, so each adds only its own
//! `pub mod` line here -- that keeps the merge between them a one-line edit
//! rather than a rewrite of this file.

pub mod no_gh;
pub mod no_local_memory;
pub mod pre_read_sym;
