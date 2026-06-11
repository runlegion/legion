//! Integration test crate for the legion binary (#608).
//!
//! One module per CLI domain, split along the section seams of the former
//! single-file tests/integration.rs. Cargo compiles this directory as a
//! single test binary, so `CARGO_BIN_EXE_legion` and the shared helpers in
//! `common` compile and link once.

mod common;

mod bullpen;
mod daemon_watch;
mod documents;
mod governance;
mod index_telemetry;
mod kanban;
mod mcp;
mod memory;
mod mesh;
mod serve;
mod task;
mod uncertainty;
mod usage;
mod worksource_pr;
