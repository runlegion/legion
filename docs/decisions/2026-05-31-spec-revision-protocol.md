# Spec-revision protocol: legitimate re-plan vs. forbidden improvisation

Date: 2026-05-31
Issue: #550 (RFC)
Parent epic: #510 (review pipeline)
Anchors: the waterfall doctrine (reflection 019e7c02), #519 (pr-write), #520 (verify)

## The question the market asked

legion's doctrine is waterfall-for-agents: design (human + agents) -> frozen SOLID issue ->
execute -> verify. The agent executes a spec it did not get to improvise around, because an
agent handed ambiguity does not discover its way out -- it improvises off a cliff (vibecoding).

The sharpest, fairest objection to this (HN 48332938, 2026-05-31 market sweep):

> "so in order to get claude to perform properly you just have to have a perfect spec up front,
> that anticipates all issues and contains no ambiguity?"

No. A perfect spec is not the claim, and demanding one would make the doctrine brittle and
dishonest. Specs are wrong sometimes. The honest claim is narrower and this RFC makes it precise:

**A spec that turns out wrong is revised through a ratified re-plan -- a new, deliberate design
cycle -- not through silent mid-flight improvisation. The waterfall forbids the second, not the
first.** Drift is forbidden; re-planning is sanctioned. The whole problem is telling them apart,
and giving the verify gate a way to tell them apart.

## The distinction

Two things look similar from a diff and are opposite in kind:

- **Improvisation (forbidden):** during execution the agent deviates from the frozen acceptance
  criteria on its own authority -- "step 3 revealed step 2 was wrong, so I just did something
  else" (digitalapplied.com names this exact 4.8 failure). No ratification. The spec on the
  board still says one thing; the work does another. This is how vibecoding launders itself past
  a waterfall: the spec exists, so it *looks* governed, but the work no longer matches it.

- **Re-plan (sanctioned):** during execution the agent discovers the spec is wrong, *stops*,
  surfaces why, and the spec is revised and re-ratified before work resumes. The board's spec and
  the work are realigned by a deliberate act, recorded, before -- not after -- the deviating code.

The difference is not in the code. It is in whether the spec change was **ratified before the
work**, and whether that ratification is **recorded** where the gate can see it.

## The protocol

1. **Detect.** During execution the agent concludes the frozen AC are wrong, incomplete, or
   unachievable as written (not merely inconvenient).
2. **Stop, do not route around.** The agent does NOT silently implement a different thing. It
   issues a re-plan request: the card moves `Accepted -> NeedsInput` with a reason. Reusing
   `NeedsInput` (not a new column) is deliberate -- it is already the "a human must weigh in"
   state, and a re-plan is exactly that.
3. **Re-ratify.** The spec (issue AC) is revised by the same design act that wrote it -- human +
   agent -- and the card returns to the work states. The revision is recorded as a
   `ReplanRecord` bound to the card: what changed, why, and that it was ratified.
4. **Resume against the revised spec.** Execution continues, now verified against the new AC.

## The audit signal (what makes it enforceable)

verify (#520) audits the work against the card's acceptance criteria. To distinguish a sanctioned
re-plan from improvisation, it needs one bit: **was this deviation ratified?**

> **The presence or absence of a `ReplanRecord` is the signal.**
>
> - Work deviates from the frozen AC **and there is no `ReplanRecord`** -> unratified deviation ->
>   verify **Fail** (this is improvisation; the work does not match the ratified spec).
> - Work matches the AC, **or** the AC were revised under a `ReplanRecord` -> verify audits
>   against the (current, ratified) AC as normal.

This is the minimal mechanism: no new database table is strictly required to *specify* it -- a
`ReplanRecord` can ride the existing card/event log -- and it gives the gate a non-skippable way
to refuse "the spec said X, I shipped Y, nobody agreed to that."

## Specified follow-up (build issue)

This RFC delivers the protocol; the build is a separate, scoped issue with checkable AC:

- `kanban::Action::ReplanRequest` -> transition `Accepted -> NeedsInput`, carrying a reason
  (mirrors the `InReview -> NeedsInput` transition added in #520). FSM test.
- A `ReplanRecord` on the card (event-log entry or a small table): `{card_id, reason,
  revised_at, ratified}`. Recorded when a re-plan is ratified.
- verify gains a deviation check: when the work diverges from the frozen AC, require a
  `ReplanRecord`; its absence is a Fail with a message naming the unratified deviation.
- `LegionError::ReplanRequired` (or a verdict reason) for the unratified-deviation case.
- Tests: ratified re-plan passes verify against revised AC; unratified deviation Fails.

## RFC-level recommendation (future, not built here)

The deeper market anxiety -- "you need a *perfect* spec" -- is best answered not by demanding
perfection but by lowering the cost of a good-enough spec and a cheap re-plan. A future
**spec-quality assist** could lint an issue at authoring time for ambiguity and missing/uncheckable
AC (the same SOLID properties pr-write and verify already depend on), shifting the failure left to
where it is cheap to fix. Scoped as future; out of scope for the follow-up build.

## Why this is the answer to "anti-drift"

The earlier framing asked "how do we keep specs from drifting in agile loops." This RFC reframes
it correctly: **we do not run agile loops, and drift is not a thing we detect after the fact -- it
is a thing we forbid by construction.** Unratified deviation Fails the gate. Ratified re-planning
is a first-class, recorded act. The spec never "drifts"; it is either honored or deliberately,
visibly revised.
