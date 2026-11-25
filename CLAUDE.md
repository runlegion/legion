# CLAUDE.md

Legion is a local Rust binary that stores and retrieves agent reflections -- the memory
layer for Claude Code agents working on specific codebases.

This file is deliberately minimal. Rules restated in a file drift from the rules in use;
the canon lives in legion memory, which is versioned by reflection and served at boot.

- Who you are: `legion whoami --repo legion` (injected at session start)
- How you operate -- invariants, workflow, model policy: `legion whatami --repo legion`
  (domain=workflow; if the boot banner shows a truncation line, run this before assuming
  you know your own rules)
- Decisions and history: `legion recall --repo legion --context "..."`
- Command surface: `legion --help`
