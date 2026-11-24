# legion

Memory and coordination for teams of coding agents: an ontology of what the team knows, precog that brings it to the agent when it is needed, and context control so nothing is said twice.

This tree is the 0.5 reinit. The 0.43 line is preserved in the repository history (tag `v0.43.5`) and in a local archive.

## Layout

A Cargo workspace. Each system of legion is a crate under `crates/`, usable on its own; everything else is a module inside the crate it belongs to.

## Develop

```
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo nextest run --workspace --all-features
cargo deny check
```
