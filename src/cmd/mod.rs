//! The legion-cmd PreToolUse adapter (#1229): the I/O boundary over the
//! pure `legion_cmd::route`. `route` itself has no filesystem, network,
//! database, or process dependency (NFR-CMD-001, FR-CMD-014) -- everything
//! in this module exists because something has to read the hook payload,
//! read the policy file, run lookups, enforce the decision deadline, and
//! write the harness response, and that something is not `route`.

pub(crate) mod config;
pub(crate) mod hook;
pub(crate) mod replacement;
