---
name: legion-review
description: |
  The review stage of the quality pipeline: simplify -> pr-write -> review -> verify. Fans
  out independent review dimensions over an open PR (spec-vs-diff, correctness, quality,
  security), adversarially verifies every HIGH/MED finding before reporting it, and records
  the result via `legion quality-gate record --skill legion-review` so downstream gates can
  require a clean review on HEAD. Run after `legion pr create`, before team sign-off and
  /legion-verify.
version: 1.0.0
user-invocable: true
allowed-tools: Bash, Read, Task
---

# Legion Review

Review an open PR against its issue spec and for code quality, with findings that survived
an adversarial check. The product is a structured decision -- approved or changes_requested --
recorded as a HEAD-keyed quality gate, plus file:line findings the implementer can act on
without guessing.

## Inputs

- The PR number (argument, or infer from the current branch via `legion pr list`).
- The repo (from the working directory's watch.toml mapping).

## Procedure

1. **Load the contract.** `legion pr view --repo <repo> --number <n>` for the body and
   branch; `legion issue view --repo <repo> --number <n>` for the linked issue's acceptance
   criteria. No linked issue is itself a finding (the workflow requires one).

2. **Fan out review dimensions.** Spawn parallel review agents via the Task tool using
   subagent_type `legion:legion-review` (plugin agents register namespaced; the bare
   `legion-review` form exists only if a repo also carries a local .claude/agents copy --
   check the available-agents list if the namespaced form does not resolve), one per
   dimension, each given the PR number, branch, issue criteria, and its single focus:
   - `spec`: acceptance-criteria-vs-diff, scope creep, claims the diff does not implement
   - `correctness`: error handling, silent failures, edge cases, concurrency
   - `quality`: repo CLAUDE.md invariants, idioms, test coverage and quality
   - `security`: injection, unchecked input, secrets, shell quoting

   For a small PR (docs-only, or under ~100 changed lines), one combined agent covering all
   dimensions is acceptable -- say so in the report.

3. **Adversarial verification.** For every HIGH and MED finding the dimensions return, spawn
   one refuter agent (subagent_type `legion:legion-review`) prompted to REFUTE the finding: read
   the cited code and argue it is wrong, mitigated elsewhere, or out of the PR's scope.
   Findings the refuter kills are dropped; findings that survive are reported. Record the
   refuted count -- a high kill rate means the dimension prompts are over-claiming.
   (Calibration data: the 2026-06 audit's refuters overturned 10 of 30 claimed findings.)

4. **Decide.** Any surviving HIGH -> `changes_requested`. Three or more surviving MED ->
   `changes_requested`. Otherwise `approved` (surviving MEDs named in the sign-off).

5. **Record the gate** (result `clean` only for approved):

   ```bash
   legion quality-gate record \
     --skill legion-review \
     --result <clean|issues> \
     --findings-count <N surviving> \
     --details-json '<JSON: decision, findings[], refuted_count, dimensions_run>'
   ```

   The gate is keyed on HEAD, so a new commit after review invalidates it -- re-run after
   fixes, exactly like the simplify gate.

6. **Report.** Emit the consolidated REVIEW REPORT (the agent definition's format): decision,
   per-dimension one-liners, surviving findings with file:line and fix suggestions, refuted
   count, sign-off.

## Findings discipline

Inherited from the legion-review agent definition and non-negotiable: every finding cites
file:line; claims the diff does not implement are HIGH; LOW findings are reported only when
zero HIGH/MED exist; no style arguments beyond the repo's own CLAUDE.md and lint gates;
no code is written or fixed by this skill -- the fix loop belongs to the implementer.

## Relationship to the other gates

- `/legion-simplify` reviews the diff's structure before the PR exists; this skill reviews
  the PR against its spec after it exists.
- `legion pr write-check` validates the implementer's own claim mapping; this skill validates
  the claims themselves against the diff, independently.
- `/legion-verify` is the per-criterion evidence gate before Done; run it after this review
  is clean and fixes (if any) have landed.
