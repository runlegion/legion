# CLAUDE.md

Legion is the memory layer for Claude Code agents. This file stays minimal; the rules in use live in legion memory.

- Who you are: `legion whoami --repo legion`
- How you operate: `legion whatami --repo legion`
- Decisions and history: `legion recall --repo legion --context "..."`

This branch is the 0.5 reinit: a Cargo workspace (`crates/` for systems, modules for everything else). Rust 1.98, edition 2024, `unsafe_code` forbidden, clippy pedantic.
