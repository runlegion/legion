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

   `findings[]`'s schema is pinned (#773) -- each entry MUST be
   `{"file": "<path>", "line": <int or null>, "severity": "HIGH"|"MED"|"LOW", "summary": "<one-line
   problem statement>"}`. This is what the finding-resolution ledger extracts and tracks toward
   resolved-or-dispositioned; an entry missing any of these keys is silently dropped from the
   ledger (the gate's own `--findings-count` stays the source of truth for the run, but that
   individual finding will never surface for disposition).

   The gate is keyed on HEAD, so a new commit after review invalidates it -- re-run after
   fixes, exactly like the simplify gate.

   **`--result clean` is refused (#773) in TWO cases, and the `approved && non-blocking findings`
   case -- the one this issue exists to close -- is the first, not the second:**
   1. **This same call's `findings[]` is non-empty.** An `approved` decision that still names
      surviving LOW/MED findings in its sign-off must NOT be recorded with `--result clean` in
      the same breath as reporting them -- record `--result issues` first (identical
      `--details-json`, so the findings are persisted), then disposition or batch-ack each one,
      THEN re-run `--result clean` with `findings[]` empty (or omit `--details-json`'s
      `findings` key entirely). A clean call cannot carry its own findings, full stop -- there is
      no "but they're only LOW" exception at this step.
   2. A HIGH/MED finding from a PRIOR review run on this branch is still PENDING (neither a later
      commit touched its file nor was it explicitly dispositioned), or a prior LOW finding is
      still un-acked. Reconciliation against git log runs automatically at record time -- a fix
      already landed in an earlier commit clears itself before this check runs.

   For anything still open in either case: `legion quality-gate finding-disposition --id
   <finding-id> --reason "won't fix: ..."` for a single finding, or `legion quality-gate
   finding-ack --branch <branch> --skill legion-review --reason "..."` to batch-clear a sweep of
   LOW findings. `legion quality-gate finding-list --branch <branch> --skill legion-review` lists
   ids and current status. An `approved` decision with non-blocking findings that are neither
   fixed nor dispositioned is exactly the hand-wave this refusal exists to stop -- do not treat
   `approved` as license to skip the disposition/ack step just because the decision itself does
   not require a fix-and-re-review round.

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
