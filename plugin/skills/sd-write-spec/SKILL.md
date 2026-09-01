---
name: sd-write-spec
description: |
  Write a scope's full requirement set -- FR and NFR documents -- from the intent/discovery
  layer, in one pass. The Deliver-edge writer: it crystallizes the 830 spec the executor
  consumes, every SHALL naming what earned it and every gap escalated rather than resolved
  in-body. Two input modes: from an intent alone (system work) or from a landed service
  design (product work). Invoke when a scope is ready to move from discovery to a buildable
  spec.
version: 0.1.0
user-invocable: true
allowed-tools: Bash, Read
---

# Spec writer: the intent/discovery layer in, a buildable 830 out

This is the seam between the two halves of the diamond. Upstream is judgment -- claims,
insights, the abductive leap. Downstream is execution -- an agent that follows the spec
without needing to have done the thinking. The spec is where the judgment becomes rules,
and its whole worth is that a rule-follower can execute it without a hole to fall into. So
the spec is complete or it is escalated; it is never left ambiguous for someone who cannot
resolve the ambiguity.

## Two laws (bind every requirement)

- **Every SHALL names what earned it.** A requirement's `traces_to` cites the source that
  makes it true: a proven toy or experiment, a supported insight (by id), a settled
  intent proposal, an ecosystem moment of truth. A candidate SHALL whose only ground is
  "the current system does this" is the let-go-of-the-past fault in spec clothing -- it is
  how the legion-cmd spec drifted into a rebuild of the guards it existed to replace.
  Unproven ground (an intent proposal with `needs_pressure_test`, or anything grounded only
  in present behavior) routes to a **RESEARCH document**, not a SHALL: build the toy first.
- **UNCLEAR goes up, never resolved in-body.** A contradiction or silent gap in the inputs
  is escalated, not decided. Land the set as far as it goes, name the gap in the affected
  requirement, and signal whoever owns the answer -- the same park pattern the pipeline
  uses. Resolving a gap in-body (labelling it DECIDED and moving on) is the exact failure
  the meaning-drift audit found in the issue-writer; this skill does not repeat it.

## Input modes

- **Intent-only** (system work -- legion-cmd is the case): the intent alone. An intent may
  or may not carry a `claims` array. When it does, each claim's `right_if` becomes
  acceptance criteria. When it does NOT (a migrated or older intent), derive from what the
  intent actually has: `direction.proposals`, `current_state`, `actors`, `boundaries`,
  `open_questions`. Do not assume the claims-bearing shape -- read the intent's own fields.
- **Service-design** (product work): the intent plus the landed ecosystem, personas,
  journeys, blueprints, and Discovery. Read every input by id first
  (`legion document view <id> --json`); the requirement set is derived from all of them,
  and each requirement traces to the specific source it came from.

There is no story layer. A requirement traces DIRECTLY to its source in the intent and the
ecosystem (decided 2026-06; reconfirmed) -- the intent's claims and the ecosystem's moments
of truth carry what a user-story layer would have, so `traces_to` points straight at them.

## The mapping

**Both modes:**

- `direction.proposals` -- a settled proposal (`status: settled`, no `needs_pressure_test`)
  becomes a SHALL; a `needs_pressure_test` proposal routes to RESEARCH, never a SHALL. A
  proposal that is `proposed` but NOT pressure-test-flagged has not earned a SHALL either:
  if the intent clearly commits to it as direction, it becomes a **SHOULD** (recommended,
  not mandatory); if it reads as a "maybe" or is genuinely undecided, escalate it and spec
  nothing on it. Only `settled` grounds a SHALL.
- `current_state` -- `real` vs planned governs tense (a SHALL for what must exist), never
  priority; a `known_gap` is a pain the requirement addresses, not a requirement itself.
- `open_questions` / any unresolved contradiction -- escalated, not resolved (law 2).
- When a resolved decision names machinery as required WHILE a `needs_pressure_test`
  proposal proposes the same machinery, split it: the settled WHAT (the outcome, the
  behavior) becomes a SHALL; the unproven HOW (the specific mechanism) routes to RESEARCH,
  and the FR notes the dependency. A resolved ruling on the outcome does not make the
  unproven mechanism a SHALL.

**Service-design mode adds** (these fields exist only when the artifacts do -- do not look
for them in intent-only mode):

- **FRs** from blueprint `backstage`/`support` mechanisms -- what the system must do.
- **Error-handling requirements** from blueprint `fail_points` (where the service breaks)
  and the ecosystem's `failure_modes` -- what must happen when it does. These populate the
  requirement's `errors` object.
- **Acceptance criteria** for a service-design FR come from the blueprint step or the
  moment of truth it serves -- the step's success condition, or the MOT's `success`, is
  what `verification.acceptance` checks. (In intent-only mode with a `claims` array, the
  claim's `right_if` is the acceptance instead.)
- **NFRs** from moments of truth and non-functional concerns. In intent-only mode, NFRs
  come from `actors` stakes, `known_gaps`, and reflections the intent cites instead.
- **Priority derived at the spec boundary and shown in `traces_to`** -- designers do not
  speak in absolutes, so the artifacts carry none; this skill assigns them and shows its
  work: a persona's `would_leave_if` backed by a supported insight -> **SHALL**; a `goal`
  the service serves -> **SHOULD**; a delighter-shaped opportunity -> **MAY**. In
  intent-only mode, priority derives from the same shape one layer up: a settled
  outcome or an actor's core stake -> SHALL, a served goal -> SHOULD, a nice-to-have -> MAY.

Every NFR needs `category` (a closed enum:
performance/scalability/reliability/availability/security/privacy/observability/maintainability/compatibility/usability/compliance
-- use `scalability` for locality, `maintainability` for policy-as-data), `metric`,
`target`, and `measurement`; an NFR without a real measurement is an aspiration, not an NFR
-- decline to invent a performance target with no way to measure it (escalate the threshold
as an open question instead).

## Procedure

1. **Read every input by id in full.** The intent always; in service-design mode the
   ecosystem, personas, journeys, blueprints, and Discovery too. A missing or invalid
   input stops the run -- you cannot spec from a half-landed artifact.
2. **Resolve the schemas** by `x-doc-type` (`requirement` and `nfr`) from
   `legion document list --doc-type schema --json`; read their `required` and `properties`
   as the contract. Requirement requires `meta`, `title`, `description`, `traces_to`
   (`meta` requires `id`, `type`, `surface`, `status`, `priority`, `owner`, `date`,
   `author`); NFR additionally requires `category`, `metric`, `target`, `measurement`,
   `verification`. `meta.priority` is the single source of a requirement's priority --
   never restate it elsewhere. Status lands `draft`.
3. **Derive the whole set for the scope in one pass**, numbered `FR-<SURFACE>-NNN` /
   `NFR-<SURFACE>-NNN`. The typed id IS the id: the store requires the storage id to equal
   `meta.id`, so you MUST pass `--id <typed-id>` on create (see step 5) -- omit it and every
   document gets a random UUID, breaking the numbering and every `depends_on`/`nfr_refs`.
   Set `depends_on` between requirements and `nfr_refs` from an FR to the NFRs that bound
   it -- these cohere only when the set is written together, which is why it is one
   invocation. `constraint_refs` points at a `constraint` doc-type that does NOT exist in
   this store: do not try to land constraints; fold a constraint-like rule into an FR and
   say why (a technology-choice SHALL, for instance).
4. **Route unproven ground to RESEARCH, escalate UNCLEAR.** A claim the inputs cannot
   ground becomes a RESEARCH document (its own doc-type -- resolve its schema by
   `x-doc-type: research`; its meta is a DIFFERENT shape from a requirement's, so template
   it off the research schema, not off an FR). The requirement cannot cite it through a
   field -- there is no `research_refs` and `traces_to` is one string that names the SHALL's
   earner. Carry the link the other way: name the dependency in the FR's `description`, and
   set the research doc's `links[].to` back to the FR. A contradiction between inputs is
   named in the requirement and signalled up, not silently resolved.
5. **Validate one of EACH doc-type before batch-creating** -- an FR, an NFR, and a RESEARCH
   sample (three different meta shapes). The store refuses a schema violation on every path;
   catching it on one document is cheaper than on the whole set. Three id/priority rules the
   schemas do not spell out:
   - A **requirement** carries priority in `meta.priority` AND you pass `--priority <same>`
     on create -- the flag populates the queryable storage column, which stays null if you
     only set the payload; keep them equal (this is a projection kept in sync, not a second
     source of truth).
   - An **NFR** carries priority only in `meta.priority`; the `--priority` flag is
     requirement-only, so the NFR's storage column stays null. That is a store limitation,
     not yours to work around -- do not omit `meta.priority`.
   - A **RESEARCH** doc has no typed id and no `meta.id`/`surface`/`owner`/`priority` -- it
     takes a UUID, so create it WITHOUT `--id` (unlike FR/NFR, which require it).

   ```
   legion document validate --schema <requirement-schema-id> --file fr-sample.json
   legion document validate --schema <nfr-schema-id> --file nfr-sample.json
   legion document validate --schema <research-schema-id> --file research-sample.json
   legion document create --doc-type requirement --id FR-<SURFACE>-001 --priority SHALL --owner <agent> --surface <surface> --from <file>
   legion document create --doc-type nfr --id NFR-<SURFACE>-001 --owner <agent> --surface <surface> --from <file>
   legion document create --doc-type research --owner <agent> --surface <surface> --from <file>
   ```

6. **Emit the predictions** (see Instrumentation below): one for the set, one per RESEARCH
   document. Emit after the documents land, so every prediction names a real id.
7. **Report** the set: the FR and NFR ids, the RESEARCH documents spun off, every gap
   escalated with who was signalled, and every prediction id emitted with its claimed
   confidence. One scope's spec per invocation.

## Instrumentation

The writer makes judgment calls the pipeline should score, and the uncertainty engine is
how a judgment becomes a track record instead of a belief. Instrument the judgments, not
the mechanics: priority and RESEARCH routing follow from the inputs' `status` fields and
are not predictions, so they get no emission. Two things are predictions.

- **The set covers the intent.** One prediction, under this skill's name, that the crit
  will accept the set without adding a requirement the inputs earned or rejecting one as
  unearned. Stake it from what you actually saw. Anchors: an intent with `claims` and a
  Discovery whose insights are `supported`, with nothing escalated, sits near 0.8; a
  claims-less intent specced from settled proposals alone sits near 0.6; a set whose
  Discovery insights are all `blocked`, or that leans on rulings and actor stakes rather
  than settled proposals, starts near 0.4. Two anchor conditions do not compound; pick the
  lowest that applies and let the escalations do the rest. Weigh the escalations rather
  than counting them. A gap named inside a requirement (FQ-4 bounding a trait registry) is
  a place the crit may reject that requirement, and lowers the number. A requirement you
  declined to write because its only ground was blocked or open is the add-risk: if a
  reader could argue the inputs earned it, the crit may add it, and that lowers the number
  too. Only an open question whose ground could earn nothing under any reading leaves it
  untouched. For the payload count, an escalation is one distinct gap named in a
  requirement's description or `errors`, or in the report; the same gap in two
  requirements counts once. Do not default to one number -- a writer that always says 0.8
  gives the estimator nothing to learn from.

  ```
  legion uncertainty emit --surface legion.sd --feature-key sd.write-spec \
    --session-id "$CLAUDE_CODE_SESSION_ID" --orphan-ttl-days 180 \
    --input-fingerprint <intent-id>:spec:<surface> --claimed-confidence <p> \
    --payload '{"intent":"<intent-id>","surface":"<surface>","mode":"<mode>","fr":<n>,"nfr":<n>,"research":<n>,"escalations":<n>}'
  ```

- **Each RESEARCH hypothesis holds.** One prediction per RESEARCH document: the probability
  the toy, once built, confirms the hypothesis. This is the only place the writer stakes a
  claim about the world rather than about its own artifact, and it is where the spread is
  real. Derive it from the inputs' evidence for the mechanism: a `current_state.real` entry
  that says the mechanism is proven in miniature, with cited issues, earns roughly 0.7 to
  0.8; a proposal argued from first principles with no experiment behind it sits near 0.5;
  a `known_gap` or open question that names it unproven at this shape pulls it toward 0.3.
  A settled sibling proposal that supports the outcome but not the mechanism is weak
  evidence: it may move the number a little, never into the next band. A narrative
  instance in a journey (one probe that worked once) is not an experiment; it may nudge
  within a band, never across one. Name the evidence you used in the payload.

  This payload carries the one free-prose field in the pipeline, so build it in a file
  rather than inline: an apostrophe in your evidence line ends the shell's single quote,
  and because emit exits 0 with no read-back it would write a mangled row rather than
  fail in front of you.

  ```
  cat > research-pred.json <<'JSON'
  {"research":"<research-doc-id>","informs":["FR-..."],"evidence":"<one line>"}
  JSON
  legion uncertainty emit --surface legion.sd --feature-key sd.research-hypothesis \
    --session-id "$CLAUDE_CODE_SESSION_ID" --orphan-ttl-days 180 \
    --input-fingerprint <research-doc-id> --claimed-confidence <p> \
    --payload "$(cat research-pred.json)"
  ```

The emit mechanics -- session id and model, the exit-0 rule, the 180-day orphan window,
non-blocking emission, never self-witnessing -- are held once in the sd-service-design
skill (Instrumentation, "Emit mechanics") and bind here. What is this writer's alone:
`<mode>` is the literal `intent-only` or `service-design`, and the report records each
prediction id with the surface string, since the crit rebuilds the set fingerprint from
intent id and surface.

**Who witnesses, and when.** A prediction nobody witnesses is an orphan and is excluded from
calibration, so the witness event is named here, not left to be discovered:

- The set prediction is witnessed by the **crit** (the acceptance step that moves documents
  past `draft`), which reads the id from this run's report and confirms it by rebuilding
  the fingerprint `<intent-id>:spec:<surface>` -- witness takes the id, never the
  fingerprint, and no lookup by fingerprint exists. `outcome_correctness` is the
  fraction of the set accepted as written, with
  label `shipped` when nothing was added or rejected, `scoped-down` when the crit cut
  requirements, and `escalated` when it sent the set back. Until the crit exists as a skill,
  the operator who accepts the set witnesses it by hand with the same rule.
- Each RESEARCH prediction is witnessed when its document's status lands `done`. Its id is
  in this run's report, and the fingerprint that confirms it is the research document's own
  id. Whoever records the finding runs `legion uncertainty witness <id> --outcome-label shipped
  --outcome-correctness 1.0` if the hypothesis held, `0.0` if refuted, and the held/refuted
  fraction of its claims when mixed. The document's `provenance.verification` counts are
  the source of that number.

## Refuses

- Minting `system-foundations` trace nodes. Foundations are a separate layer -- new
  verbs and capabilities that have nothing to do with the intent/discovery layer -- so when
  a requirement needs a trace target that does not exist, surface it and ask; the node is
  created in the foundations layer on its own terms, never as a side effect of a spec run.
- Writing issues. The spec is the requirement set; turning requirements into issues is the
  issue-writer's job, downstream.
- A SHALL grounded only in "the current system does this," or any requirement with no
  `traces_to` source -- unproven ground routes to RESEARCH.
- Landing a `constraint` document -- the doc-type does not exist here; a constraint becomes
  an FR.
- Resolving an input contradiction in-body. It escalates.
- Any status beyond `draft`. Acceptance is the crit, a separate step.
- Witnessing its own predictions. The writer stakes them; the crit and the research finding
  score them. A self-witnessed prediction is the rubber stamp the engine exists to catch.
