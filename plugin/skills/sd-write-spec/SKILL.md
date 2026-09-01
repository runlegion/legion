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
  intent proposal, a settled boundary rule, an ecosystem moment of truth. A candidate SHALL whose only ground is
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
- `boundaries` -- an entry whose `note` states a rule the scope must hold to ("must never
  depend on X", "the dependency runs one way") is a settled constraint and earns a SHALL,
  folded into an FR since no constraint doc-type exists. An entry that only assigns
  ownership is scoping, not a requirement: when it hands a capability to another owner,
  that capability is out of this scope, and a requirement that would cross the line is the
  boundary violation to name and escalate rather than spec.
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
   `NFR-<SURFACE>-NNN`. The typed id IS the id, so you MUST pass `--id <typed-id>` on
   create (see step 5) -- omit it and every document gets a random UUID, breaking the
   numbering and every `depends_on`/`nfr_refs`. The store does not check the two against
   each other: it takes the storage id from the flag and never reads `meta.id`, so a
   mismatched pair lands silently. The storage id is a projection of `meta.id` kept in
   sync by you, not a second source of truth.
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

6. **Report** the set: the FR and NFR ids, the RESEARCH documents spun off, and every gap
   escalated with who was signalled. One scope's spec per invocation.

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
