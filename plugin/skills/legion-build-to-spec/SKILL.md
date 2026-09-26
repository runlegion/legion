---
name: legion-build-to-spec
description: The orchestrator's gate at the pen. Run it before you send a brief to an implementer or a gate, create or edit an issue, record a finding disposition, or tell the operator what the spec requires. Every behavior claim in that text quotes the spec text it comes from; a mechanism the spec does not name never goes into a brief or an issue; acceptance text is frozen once a build starts. When reality and the spec disagree, you do not pick the answer -- you write a spec delta for the operator and build as written or park. A claim that fails is a stop -- HALT, do not send.
---

# Build to spec

The failure this stops: the spec is written, the build starts, and reality pushes back -- a review finding, two issues that conflict once both merge, a case the requirement never named. Each push gets answered in the text the orchestrator happens to be writing: a builder brief grows a mechanism, a disposition invents a behavior, an issue's criteria get rewritten to match the code, a heading gets renamed so a gate stops refusing. Every step is locally reasonable. After enough of them the code, the issues and the requirements describe three different systems, and nothing fits -- the way waterfall fails on big projects.

The shape will change during a build. That is not the failure. The failure is absorbing the change silently, in the orchestrator's own words, where no requirement records it and every later gate grades against it.

The rule is already in memory. Memory is read at boot; this happens mid-run, at the pen. So the check runs at the pen.

## When it runs

Before any of these leave you:

- a brief to an implementer, a reviewer, or a verifier
- `legion issue create` or `legion issue edit`
- a finding disposition (`legion quality-gate finding-disposition`, `finding-ack`)
- a statement to the operator of what the spec requires or allows

## The checks, per claim

A claim is any sentence that says what the system does, must do, or must not do. Run all four on each.

1. **Quote the source.** The claim carries the spec text it comes from, quoted: the requirement id and the acceptance line or description sentence, or the issue section and its words. "FR-CMD-025 requires built-in wrapper resolution" stops. "FR-CMD-025 acceptance: 'wrapper variants of a no-go entry are refused: sudo rm -rf /, env rm -rf /, ...'" carries. A paraphrase in place of the quote is a stop -- the paraphrase is where the drift gets in.
2. **No mechanism the spec does not name.** A data structure, a file, a field, a split of a budget, a second pass, an algorithm, a new policy key: if the spec does not name it, it does not go into a brief or an issue's Requirements. State the behavior the spec requires and the evidence that it is not met; the builder picks the mechanism inside the spec, and review judges it. A brief that says "copy the declarations into a table in nogo.rs" has already designed the fix.
3. **Acceptance text is frozen.** Once a build against an issue has started, its Goal, Requirements and acceptance text are the operator's to change, not yours. Editing them to match what was built is the laundering shape: every gate after the edit grades the work against your conclusion. Adding facts that do not change what is required (a line reference, a link, a reproduction) is allowed; say so in the edit.
4. **No gate workarounds.** When a gate refuses (a trace it cannot resolve, a criterion it cannot check), the refusal is information. Renaming a heading, rewording a criterion, or narrowing a brief so the gate passes hides it. Report the refusal as what it is.

## When reality and the spec disagree: the spec delta

A finding the spec does not settle, two issues that conflict, a requirement that cannot be met as written, a case no requirement names -- these are spec deltas. You do not pick the answer. You write it down where the operator decides it:

- **Where:** a GitHub issue labeled `spec-delta`, via `legion issue create`. It is the existing issue store; do not invent another, and do not put it in memory -- a reflection reads as authority to every later gate.
- **What it holds:**
  - the spec text on each side, quoted, with ids
  - the evidence: the finding, the test, the reproduction
  - the gap, in one sentence
  - what is blocked meanwhile, and what you are doing instead
  - options, if you see them, labeled as options -- never as the decision
- **Meanwhile:** build as written, or park the item. Never build your preferred answer ahead of the operator's.

A delta is not a failure to report. It is how the spec keeps fitting the system: every change the build discovers lands in the requirements through the operator, so at the end the spec still describes what shipped.

## What you decide without asking

The operator does not want every choice brought back. These stay yours, as long as each stays inside the spec:

- a finding disposition that keeps the spec as written: fix it, defer it to an issue, or record why it is not a defect
- sequencing, merge order, rechecks after a merge
- which problems get follow-up issues -- stated as the problem, the measurement, and the spec text it breaks, never the fix
- how to run the pipeline: which agent, which worktree, which gate next

If a choice needs a claim that fails the checks above, it is not yours. It is a spec delta.

## A stop is a HALT

A claim that fails a check is a stop. Do not send the brief, file the issue, or record the disposition until every claim quotes its source or has become a spec delta. Do not smooth a failing claim into softer wording; softer wording is still the claim.

## Bounds

- It does not judge whether the spec is right. The operator does, through spec deltas.
- It governs your own text. An implementer's commit, a reviewer's finding and a verifier's verdict are theirs; if one of them carries a mechanism the spec does not name, that is review's call, and a spec delta if it changes what is required.
- It does not slow ordinary work: a brief that restates an issue's own acceptance lines and names the worktree passes every check.
