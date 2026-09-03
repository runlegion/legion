---
name: Implementation Task
about: Implementation task for AI agents with full dev workflow
title: "feat: [brief description]"
labels: enhancement
assignees: ''
---

## Goal

**Single, focused objective this task achieves.**

## Requirements

### Interface
```rust
// Exact struct definitions, trait signatures, or API expected
```

### Behavior
- Specific requirement 1 with clear success criteria
- Specific requirement 2 with measurable outcome
- Specific requirement 3 with validation method

### Error Handling
- What errors to return and when
- Required error types and messages

## Out of Scope

- Feature 1 (separate issue)
- Feature 2 (future consideration)

## File Locations

- Implementation: `src/module_name.rs`
- Tests: Bottom of same file in `#[cfg(test)]` module

## Dev Workflow

Each step is mandatory. Do not skip steps or combine them.

1. **Build** -- Implement the feature. Write tests alongside code. Run `cargo test`, `cargo clippy -- -D warnings`, `cargo fmt -- --check`. All must pass.
2. **Simplify** -- Run `/legion:legion-simplify` on all changed files. Accept structural improvements, flatten unnecessary abstractions, remove dead code. This records the `legion-simplify` gate that `legion pr create` requires; the harness `/simplify` skill is a different tool and records no gate.
3. **Review** -- Run `/legion:legion-review`, which fans out parallel review dimensions (spec-vs-diff, correctness, quality, security) and adversarially verifies each finding. Do not create the PR yet.
4. **Fix** -- Address every issue the review found. Re-run tests after fixes.
5. **PR** -- Create the PR. Reference this issue number.
6. **Verify** -- Run `/legion:legion-verify` (or `legion verify --issue <this issue>`). It reads the acceptance criteria, the diff, and the test output and emits one verdict per criterion against the **traced requirement**, not the issue's restatement of it, and it **witnesses every prediction emitted for this work** -- an emitted-but-unwitnessed prediction is an orphan, and verify is the stage that ends orphans. All-pass records a clean verify gate; any fail hard-blocks; any uncertain needs a human. Run this after Review, before the issue is closed -- it is the independent net, and skipping it is skipping the step.

### Rust Rules
- No `unwrap()` in production code
- No `unsafe` code
- No emoji in code, comments, or documentation
- `cargo clippy -- -D warnings` must pass
- `cargo fmt -- --check` must pass
- Errors use thiserror derive macros

## Done When

- [ ] All tests pass
- [ ] Simplify pass completed
- [ ] Review pass completed and issues fixed
- [ ] PR created and linked to this issue
- [ ] Verify pass recorded (all criteria pass against the traced requirement)
- [ ] Predictions witnessed (the uncertainty chain is closed -- no orphans left by verify)

**This issue is complete when:** [Specific, measurable completion condition]

## Context

- Related issues: #N, #M
- Design docs: link if applicable
