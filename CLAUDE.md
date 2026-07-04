# CLAUDE.md

Legion is a local Rust binary that stores and retrieves agent reflections. It is the memory layer for Claude Code agents working on specific codebases.

## Technical invariants (this repo)

- No emoji in code, comments, or documentation
- No `unwrap()` in production code
- No `unsafe` code
- All types explicit
- Errors use `thiserror` derive macros
- UUIDv7 for all IDs
- `cargo clippy -- -D warnings` must pass
- `cargo fmt -- --check` must pass

## Dev workflow (mandatory)

1. **Plan** -- propose approach, get Sean's confirmation before coding.
2. **Issue** -- `legion issue create --repo legion --title '...' --body '...'`.
3. **Build** -- branch `feat/<issue#>-<short-desc>`. Tests alongside code. `cargo test`, `cargo clippy -- -D warnings`, `cargo fmt -- --check` all pass.
4. **Simplify** -- run `/legion-simplify` on the branch. Records its result via `legion quality-gate record`.
5. **PR** -- `legion pr create`. Must reference the issue. `legion pr create` requires a clean simplify gate on HEAD.
6. **Review** -- run `/legion-review` on the PR: fans out review dimensions, adversarially
   verifies findings, records the gate. This replaces team review -- do not request
   vault/smugglr reviews.
7. **Fix** -- address every finding. Re-run tests after fixes; re-record gates on the new HEAD.
8. **Verify** -- run `/legion-verify` against the card's acceptance criteria before Done.
9. **Consensus** -- for big changes, post to the bullpen and get team input before merging.
10. **Ask for merge** -- never merge to main without explicit user approval.

## Reach for these when you need more

- `legion --help` for the full command surface (CLI reference).
- `docs/site/architecture.md` for schema, sync model, indexing.
- `docs/plans/` for in-flight design documents.
- `legion recall --repo legion --context "..."` for accumulated decisions and lessons.

Identity, voice, doctrine, and universal rules live in your identity reflection -- they surface automatically at session start via the SessionStart whoami banner. This file holds only repo-specific technical invariants and the workflow.
