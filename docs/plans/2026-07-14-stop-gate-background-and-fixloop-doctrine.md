# Stop-gate awareness of harness-native background work + orchestration-template fix-loop doctrine

**Date:** 2026-07-14
**Status:** Design doc -- Part 1 is forward-looking (no sound signal exists yet); Part 2 is
adoptable doctrine, effective on any template touched from now on.
**Issue:** #788
**Companions:** #778 (reduced `Delegated` design, watch-spawned only), #773 (finding-resolution
gate)

## Why this doc exists

Both halves below were split out of #778 and #773 during the 2026-07-14 plan groom because
neither had a landing spot: #778's reduced design deliberately scoped `CardStatus::Delegated` to
watch-spawned PTY work only, leaving harness-native backgrounding undesigned; #773's
finding-resolution AC referenced a "wave/workflow template" doctrine that no single artifact in
this repo owned. This doc is that landing spot for both, kept together because they were split
from the same groom on the same day, not because they are technically related. They are read
independently below.

---

## Part 1 -- Stop-gate awareness of harness-native background work

### The gap this doc tracks

#778 binds `CardStatus::Delegated` to the #679 liveness substrate (`wake_attempts` +
`persona_wake_leases`, `LIVE_LEASE_WHERE` at `src/db/wake.rs:61`), which is populated only by
`legion watch`'s PTY spawn path (`src/watch/spawn.rs:314-334` stamps `LEGION_SPAWN_SOURCE=watch-pty`
and, when given an attempt id, `LEGION_WAKE_ATTEMPT_ID`). That substrate is unfakeable *for that
one case*: entry into `Delegated` requires watch to be actively running for the repo and a live
`wake_attempts` row for the card, so a bare status flip cannot manufacture it.

Work backgrounded a different way -- a Task-tool subagent spawned inside the current Claude Code
session, or any other harness-native "keep working while I do something else" primitive -- has no
such row. `legion watch` was never involved; nothing wrote to `wake_attempts`. From the Stop
hook's point of view (`plugin/hooks/stop.sh`), this work is invisible: the Stop payload exposes
only `.cwd`, `.tool_name`, `.session_id`, `.stop_hook_active` (`plugin/hooks/lib/prelude.sh` +
`stop.sh`), and the harness surfaces no background-task registry to hooks. An agent holding an
Accepted card with a live subagent working on it hits the in-progress gate
(`stop.sh`'s `ACCEPTED` block, reading `kanban list --json | select(.status == "accepted")`)
exactly as if it had abandoned the card.

This is deliberately not "fix it now." #778's crux still binds here word for word: **a free,
self-set label is a strictly-worse `LEGION_SKIP_STOP_BLOCK`.** Any design that lets an agent
claim "I backgrounded this" without a signal the harness (not the agent) produces is the exact
bypass-with-a-fig-leaf #778 was built to refuse. So the honest position today is: harness-native
background work gets **no** `Delegated` exit. The agent uses the real states that already exist --
stay Accepted and let the gate block until the subagent's result lands in the same turn (the
common case for Task-tool subagents, which are typically awaited before Stop fires), or hand the
card to `needs-input`/`blocked` if it is genuinely stuck. This doc's job is to track what signal
*could* change that, and pre-specify the design that would consume it, so adopting a real signal
later is a wire-up, not a re-design.

### Candidate signals, graded against #778's two constraints

A candidate is sound only if it is (a) a liveness signal the agent cannot fabricate, and (b)
bounded -- the card cannot sit in a background-delegated state forever; it must auto-revert when
the background work completes or dies.

| Candidate | Unfakeable? | Bounded? | Verdict |
|---|---|---|---|
| **Harness-exposed background-task registry.** A hypothetical future Claude Code feature where the Stop payload (or a queryable API) lists live Task-tool/background invocations for the session. | Yes, if the harness (not agent memory) produces it -- same trust class as `wake_attempts`. | Yes, if the registry entry disappears when the task ends -- ties auto-revert to the harness's own bookkeeping. | **Sound, not available.** This is the shape to build against once the harness ships it. Nothing in Claude Code today exposes this to hooks (verified against the current Stop payload shape, same audit as #778). |
| **Daemon-observed transcript/task-file freshness.** `legion watch`'s daemon (already polling on `tick_health`, `src/watch/mod.rs:241`) watches the session's transcript file or a task-status file CC writes for the session, and infers liveness from mtime/content deltas. | Partial. The daemon observes real filesystem state the agent does not directly control, but "freshness" is a proxy, not a direct liveness assertion -- a stalled-but-alive subagent and a dead one can both look stale within one poll window, and an agent could in principle nudge the file without doing the work it names. | Yes -- staleness past a threshold reverts the card, same shape as `reap_finished`. | **Weaker than the registry case, but buildable today** if CC's transcript/task-file format is stable enough to key off of. Needs a concrete file-format contract before this stops being speculative; not specified further here because that contract does not exist yet. |
| **CC changelog watch.** A meta-process that watches Claude Code's own release notes / changelog for a background-task-visibility feature landing, and files a legion issue when it does. | N/A -- this is not a liveness signal itself, it is a trigger to revisit this doc. | N/A | **Process, not a design.** Worth stating as the trigger condition for reopening this doc: when CC ships background-task visibility (registry or an equivalent), file a follow-up issue against this doc's design (below) rather than re-deriving it. |

None of these is both sound and available today. That is the honest current state, not a gap in
this doc.

### The design to consume a sound signal, once one lands

Whichever candidate above becomes real, the consuming design should **mirror #778's mechanism**,
not invent a parallel one -- the two cases (watch-spawned, harness-native) are the same shape
(bounded delegation to a liveness-checkable delegate) with different liveness sources:

1. **Same `CardStatus::Delegated` and `Action::Delegate`** (`src/kanban/state.rs`'s closed enum
   and exhaustive transition table, `CardStatus` at lines 15-24, `transition()` at 132-195) --
   not a second status. The state means "work is running elsewhere and is liveness-checked,"
   regardless of which substrate proves the liveness.
2. **A second liveness predicate, gated the same way #778 gates the first.** Entry into
   `Delegated` from a harness-native background source requires the sound signal (a live registry
   entry, or fresh-enough transcript/task-file state) to be present *at entry time* -- exactly
   how #778 requires an active `legion watch` run and a live `wake_attempts` row. A card cannot
   flip to `Delegated` on the strength of "trust me, I background-spawned something."
3. **Auto-revert riding the existing sweep**, `tick_health` (`src/watch/mod.rs:241`, already
   driving `reap_finished`), the same loop #778 uses for the watch-spawned case -- not a new
   timer. When the harness-native liveness signal goes stale or disappears, the card reverts to
   `Accepted` and is telemetried, re-waking the agent.
4. **`stop.sh` gains an additional liveness check alongside the one #778 adds**, following the
   repo's existing forward-compatible-dead-code pattern: `LEGION_SPAWN_SOURCE=watch-pty` and the
   `session_end_handoff` function (`plugin/hooks/stop.sh`) are both wired in ahead of their
   producer landing, and stay inert (`|| true`, guarded on the env var / registry being present)
   until the producer exists. The harness-native liveness check should land the same way: dead
   code behind an `if` on the new signal's presence, so the wire-up is a small diff once the
   signal is real rather than a redesign.
5. **Honest limitation, stated up front like #778's:** auto-revert is only as good as the sweep
   that drives it. #778 documents this as "guaranteed only while `legion watch` runs for that
   repo." The harness-native case inherits the same shape: auto-revert is guaranteed only while
   whatever process observes the liveness signal (watch's daemon, most likely) is running for
   that repo. That is a precondition to document, not a silent degradation to paper over.

### What this doc deliberately does not do

- It does not add a `Delegated` exit for harness-native work today -- no sound-and-available
  signal exists (see the grading table).
- It does not propose polling Claude Code's undocumented internals for background-task state;
  that would itself be an unfakeable-signal violation in the other direction (fragile, liable to
  silently stop working across harness versions, and not something the agent or the daemon can
  assert liveness from reliably).
- It does not touch `plugin/hooks/stop.sh` or `src/kanban/state.rs`. Those changes are #778's
  scope (watch-spawned case) and, later, a follow-up issue once a sound harness-native signal
  exists.

---

## Part 2 -- Orchestration-template fix-loop doctrine

### The rule

**An orchestration template's fix loop must act on every finding above the triviality threshold,
not only the ones that block approval.** "Act on" means one of:

- **Resolved** -- a subsequent step in the same run demonstrably touches the flagged file/line
  (a fix commit, in the Rust-pipeline sense #773 is building toward).
- **Dispositioned** -- an explicit, recorded reason the finding is not being fixed ("won't fix:
  intentional, see X"), not a silent drop.
- **Batch-acknowledged** -- for LOW/cosmetic findings only, a single one-line ack covering the
  batch is enough; the point is a conscious sweep, not per-nit ceremony (mirrors #773's
  triviality-threshold AC).

A template whose fix loop triggers only on `changes_requested` (or HIGH-severity, or a MED count
above a threshold) and treats every other outcome as "approved, done" structurally drops findings
that were written down but never acted on. That is not a hypothetical: it is the shape of the bug
below, on a real template in this repo.

### The exemplar this doctrine names

`.claude/workflows/wave2.js` is the wave orchestration template currently used to run implementer
-> gate -> review pipelines in this repo (the `#773`-groom search that reported "no such artifact"
missed it because it lives under the `.claude/` dotdir, not `plugin/agents/` or a top-level docs
path). Its review-and-fix loop, lines 166-174:

```js
const blocking = review.findings.filter(f => f.severity === 'HIGH').length > 0 || review.findings.filter(f => f.severity === 'MED').length >= 3
if (review.decision === 'approved' && !blocking) {
  // ... records the gate as clean and returns ...
}
```

When the reviewer returns `decision: 'approved'` with findings that are non-empty but not
`blocking` (any number of LOW findings, or up to two MED findings), the loop takes the `approved
&& !blocking` branch: it records `legion quality-gate record --skill legion-review --result clean
--findings-count <n> --details-json '<...>'` (line 170) and returns. The findings are preserved as
free text inside that one `details` JSON blob (the `quality_gates.details TEXT` column, `src/db/
quality_gates.rs:21-32` -- #773's Build Notes note this is the *only* place findings live today,
there is no findings table), but nothing in the loop ever revisits them. The fix branch (lines
176-193, the `log(...)` + fix `agent(...)` call) is reached only when the `if` above is false --
i.e. only on `changes_requested` or blocking severity. A run that returns `approved` with three LOW
findings and one MED finding exits clean with all four untouched. This is the same failure mode
#773's Problem section cites as proven live on wave 5/6/7 (three merged, un-actioned findings), now
located in the control flow that produces it.

This doc does not modify `wave2.js`. Wiring this doctrine into it is separate follow-up work (a
template touched under this doctrine, not the doctrine itself); flagged here as located evidence
for why the doctrine is not academic.

### The contract templates must satisfy

Any orchestration template with a review-then-fix loop (wave templates, workflow scripts, or any
future equivalent) must structure that loop so that:

1. **The branch that skips the fix step is keyed on "no actionable findings remain," not on
   "decision was approved."** `decision` (approved / changes_requested) and blocking-severity are
   inputs to *how urgently* the loop must act, not gates on *whether* it acts at all. A template
   may still special-case severity -- e.g. only entering a full fix-and-re-review round for
   HIGH/blocking findings -- but every non-blocking finding must still be routed to resolved,
   dispositioned, or batch-acked before the run records the gate as clean. Recording clean while
   an un-dispositioned finding sits only in a details blob is the violation.
2. **Disposition is recorded, not implied.** If a template chooses not to fix a non-blocking
   finding in the current round, it writes down why (a `disposition_reason`-shaped field, even if
   today's schema is just the `details` JSON blob) before recording the gate. Once #773 lands the
   findings table and commit-linked resolution detection it describes (its Build Notes: a table
   keyed to `quality_gates.id` with file/line/severity/status/disposition_reason/
   resolved_by_commit), a template's `quality-gate record` call is the natural place to persist
   this -- the template doctrine and #773's schema are meant to converge there, not run as two
   parallel bookkeeping systems.
3. **Batch-ack is explicit and bounded.** A one-line ack that covers "N LOW findings, batch-
   acknowledged: <reason>" is acceptable *only* for findings below the triviality threshold
   (formatting, comment density -- the same class #773 names). It is not a rubber stamp for
   MED/HIGH findings that merely failed to cross the current `blocking` threshold.
4. **The loop's own gate-recording call must not run past step 1-3.** Concretely for `wave2.js`'s
   shape: the `legion quality-gate record ... --result clean` call at line 170 must not execute
   until every finding in `review.findings` (not just the ones that made the round `blocking`)
   has been routed through resolve/disposition/batch-ack.

### Interaction with #773's enforcement

#773 is building the enforcement half of this for the Rust `quality-gate` pipeline:
`legion quality-gate record` (for `legion-simplify` and `legion-review`) will refuse `clean` while
non-trivial findings from that same run are neither resolved nor dispositioned, sitting on top of
a new findings table and git-log-based resolution detection (`src/cli/verify.rs`'s
`handle_quality_gate`, the `Record`/`Check` arms at roughly lines 131-155 and 198-271 per #773's
Build Notes). That refusal is a second, independent enforcement surface from the one this doc's
contract targets: #773 makes the *CLI* refuse to record clean; this doc's contract is about the
*template's control flow* deciding whether it even attempts to resolve/disposition findings before
calling that CLI.

They compose, and the composition matters: once #773 lands, a template like `wave2.js` that still
takes the `approved && !blocking` shortcut will find its line-170 `quality-gate record` call
*refused* by the CLI (non-trivial findings unresolved), not silently accepted. At that point the
template's control flow -- not just its recording call -- must change: the loop needs a triage
path (resolve small fixes inline, disposition the rest with a reason, batch-ack the trivial
findings) that runs *before* the record call, for both the `approved` and `changes_requested`
cases, so the record call has something to point to instead of retrying an unchanged blob against
a CLI that will keep refusing it. This doc specifies that triage path is required; #773 specifies
the schema and refusal mechanism it is required to satisfy.

### What this doc deliberately does not do

- It does not modify `.claude/workflows/wave2.js` to add the triage path -- that is template code,
  not design doctrine, and is out of scope for a docs-only issue.
- It does not build the findings table, structured extraction, or resolution detection -- that is
  #773's scope, referenced above, not duplicated here.
- It does not specify UI/CLI surface for querying disposition history (`legion quality-gate ...`
  flags) -- also #773's scope.

---

## Acceptance mapping

- **docs/plans/ design doc merged covering both halves** -- this file, Part 1 (harness-native
  backgrounding) and Part 2 (fix-loop doctrine).
- **#778 and #773 reference it** -- addressed by editing both issue bodies to add a "Companion
  design doc" pointer to this file's path (`docs/plans/2026-07-14-stop-gate-background-and-fixloop-doctrine.md`),
  done alongside this doc's merge.
