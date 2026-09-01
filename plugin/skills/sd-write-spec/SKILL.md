---
name: sd-write-spec
description: |
  Write a scope's full requirement set -- FR and NFR documents -- from the intent/discovery
  layer, in one pass. The Deliver-edge writer: it crystallizes the IEEE 830-style requirement set the executor
  consumes, every SHALL naming what earned it and every gap escalated rather than resolved
  in-body. Two input modes: from an intent alone (system work) or from a landed service
  design (product work). Invoke when a scope is ready to move from discovery to a buildable
  spec.
version: 0.1.0
user-invocable: true
allowed-tools: Bash, Read
---

# Spec writer: the intent/discovery layer in, a buildable requirement set out

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
  requirement, and signal whoever owns the answer with
  `legion signal --repo <your-repo> --to <owner> --verb question --note "<the gap>"`, one
  signal per owner, and list every signal sent in the report. The report alone is not the
  signal: an owner who is not woken never answers. This is the same park pattern the
  pipeline uses. Resolving a gap in-body (labelling it DECIDED and moving on) is the exact failure
  the meaning-drift audit found in the issue-writer; this skill does not repeat it.

## Input modes

- **Intent-only** (system work -- legion-cmd is the case): the intent alone. An intent may
  or may not carry a `claims` array. When it does, each claim's `right_if` becomes
  acceptance criteria. When it does NOT (a migrated or older intent), derive from what the
  intent actually has: `direction.proposals`, `current_state`, `actors`, `boundaries`,
  `open_questions`. Do not assume the claims-bearing shape -- read the intent's own fields.
- **Service-design** (product work): the intent plus the landed ecosystem, personas,
  journeys, blueprints, and Discovery. "Landed" means the document exists in the store and
  validates against its schema; `draft` status is fine. Read every input by id first
  (`legion document view <id> --json`); when the caller supplies only the surface, find the
  rest with `legion document list --doc-type <type> --surface <surface> --json`. An
  archived predecessor of another doc-type (a painmatrix) is not a Discovery; migrate it
  first. The requirement set is derived from all of them, and each requirement traces to
  the specific source it came from.

A missing artifact and a gap inside an artifact are different failures. A missing or
schema-invalid artifact PARKS the run; step 1 says what parking means. A gap inside a
landed artifact is law 2: land the set, name the gap, escalate. A weak artifact (a Discovery whose insights
are all `blocked`) is landed, not missing; the run proceeds and the verdicts decide what
earns what.

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
  priority. A `real` entry that records a proven experiment with cited sources (an audit
  corpus with reproductions, a probe script that found real defects) IS law 1's "proven
  toy or experiment" and earns a SHALL; a `real` entry that merely describes the present
  system earns nothing. A `known_gap` is a pain the requirement addresses, not a
  requirement itself; it may ground an NFR's `traces_to` when an actor's stake says the
  gap must be closed.
- `open_questions` -- an unresolved one, or any unresolved contradiction, is escalated,
  not resolved (law 2). A resolved one whose `resolution` records an operator ruling ranks
  as `settled`: the outcome it names earns a SHALL. A field elsewhere in the intent that
  still contradicts that resolution (a settled proposal that assumes cancelled machinery)
  is an UNCLEAR, not a fact; name it and escalate it to the intent's owner.
- When a resolved decision names machinery as required WHILE a `needs_pressure_test`
  proposal proposes the same machinery, split it: the settled WHAT (the outcome, the
  behavior) becomes a SHALL; the unproven HOW (the specific mechanism) routes to RESEARCH,
  and the FR notes the dependency. A resolved ruling on the outcome does not make the
  unproven mechanism a SHALL.

**Service-design mode adds** (these fields exist only when the artifacts do -- do not look
for them in intent-only mode):

- **Discovery insights, by `status`** (the schema's verdict field). `supported` earns a
  SHALL for the need it evidences. `bounded` earns a SHALL whose scope is the bound the
  insight states, and nothing beyond it. `contradicted` earns nothing and cancels any
  candidate whose only ground it was; say so in the report. `blocked` and
  `saturated-unevidenced` are unproven ground: they earn nothing and kill nothing, a
  requirement with another earner stands, the insight may be cited in a description, and
  its `next_probe` folds into the RESEARCH document for the mechanism it would test.
- **FRs** from blueprint `steps[].layers` (`backstage`, `support`) -- what the system must
  do -- and from the ecosystem's `moments_of_truth`: a moment of truth's `success` text
  earns a SHALL; its `failure` text is what the `errors` object guards against and earns
  nothing on its own. A persona's own `moment_of_truth` supports a citation; only the
  ecosystem's earn.
- **Error-handling requirements** from blueprint `steps[].fail_points` (where the service
  breaks) and the ecosystem's `failure_modes` -- what must happen when it does. These
  populate the requirement's `errors` object. A step's `frictions` (customer hurt; the
  field that superseded `pain_points`, which older blueprints still carry) are what the
  requirement must relieve, not where it breaks. A blueprint carrying none of these leaves
  `errors` to the ecosystem's `failure_modes` alone. A failure mode whose `recovery` is
  undecided is an UNCLEAR carried in `errors`, not a rule.
- **Acceptance criteria** for a service-design FR come from the blueprint step or the
  moment of truth it serves -- the step's success condition, or the MOT's `success`, is
  what `verification.acceptance` checks. In intent-only mode with a `claims` array, the
  claim's `right_if` is the acceptance. In intent-only mode WITHOUT claims, acceptance
  comes from the proposal's own text and the intent's cited evidence: each criterion names
  one observable, and where the intent cites a defect issue with a reproduction, the
  pre-fix reproduction is a criterion (the harness must catch it).
- **NFRs** from moments of truth and non-functional concerns. In intent-only mode, NFRs
  come from `actors` stakes, `known_gaps`, and the source notes in `meta.sources` (a cited
  reflection is read through its note there; there is no command to view a reflection as a
  document).
- **Priority derived at the spec boundary and shown in `traces_to`.** Personas carry
  `needs[].priority` (SHALL/SHOULD/MAY) and the skill does not honour them as earners: a
  persona-stated SHALL is a claim to check, and the earner is the supported insight or
  moment of truth behind it. When none exists, escalate the need; do not spec it on the
  persona's word. What earns what: a supported insight or a moment of truth's success ->
  **SHALL**; the journey's `meta.goal`, served by a step with no insight behind it ->
  **SHOULD**; a journey row's `opportunities` entry or a persona need the inputs mark as a
  delighter -> **MAY**. In intent-only mode, priority derives from the same shape one layer
  up: a settled outcome, a ruling, or an actor's core stake -> SHALL, a served goal ->
  SHOULD, a nice-to-have or an uncommitted consumer -> MAY.
- **A pre-existing spec on a sibling surface** is not an input and is not superseded by
  this run. Read it, report every requirement of yours that overlaps one of its, and every
  trace of its that now points at an archived or blocked ground. Whether the old set is
  retired is the crit's call.

### The `traces_to` grammar

`traces_to` is one string: the source tokens that earned the requirement, then the
derivation: `<token>[; <token>...] -- <PRIORITY> because <rule>`. Tokens are
`intent.<json-path>` (e.g. `intent.direction.proposals[3]`,
`intent.open_questions.FQ-1.resolution`), `discovery.<insight-id>`, `moment_of_truth.<n>`,
`blueprint.step.<n>`, `journey.phase.<n>`, `persona.<slug>.needs[<i>]`, and
`boundary.<owner>`. The first token is the earner; the rest support. `meta.priority` holds
the value; `traces_to` shows how it was derived. That is one source, shown twice, not two
sources.

Every NFR needs `category` (a closed enum:
performance/scalability/reliability/availability/security/privacy/observability/maintainability/compatibility/usability/compliance
-- use `scalability` for locality, `maintainability` for policy-as-data), `metric`,
`target`, and `measurement`; an NFR without a real measurement is an aspiration, not an NFR
-- decline to invent a performance target with no way to measure it (escalate the threshold
as an open question instead). A target of zero that is the failure mode's own definition
(zero leaked handles, zero differing verdicts) is not invented; a count or a duration with
no number in the inputs is.

## Procedure

1. **Check every input exists and validates, then read each by id in full.** The intent
   always; in service-design mode the ecosystem, personas, journeys, blueprints, and
   Discovery too. Do the existence check before the reading: a missing or schema-invalid
   input parks the run, and there is nothing to read 60KB of inputs for. Parking means:
   land nothing, write the report naming the missing input and its unblock, and signal the
   surface's owner (`--verb request`). Do not treat the intent's `meta.sources` as inputs;
   they are provenance, read through the intent's notes.
2. **Resolve the schemas** by `x-doc-type` (`requirement` and `nfr`) from
   `legion document list --doc-type schema --json`; read their `required` and `properties`
   as the contract. Requirement requires `meta`, `title`, `description`, `traces_to`
   (`meta` requires `id`, `type`, `surface`, `status`, `priority`, `owner`, `date`,
   `author`); NFR additionally requires `category`, `metric`, `target`, `measurement`,
   `verification`. `meta.priority` is the single source of a requirement's priority value;
   `traces_to` shows the derivation (see the grammar above) and nothing else repeats it.
   Status lands `draft`. The `list` output's `payload` is a JSON string; parse it a second
   time to reach `x-doc-type`.
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
   it off the research schema, not off an FR). The shape, so you need not read the whole
   schema to start: `finding` is one sentence and for a not-yet-built toy reads
   `UNTESTED: <hypothesis>`; every entry in `claims` is `unverified`;
   `provenance.verification` counts them as `unverifiable`; `meta.status` is `draft` (its
   enum is draft/review/done, not the requirement's). The requirement cannot cite it
   through a field -- there is no `research_refs` and `traces_to` is one string that names
   the SHALL's earner. Carry the link the other way: name the dependency in the FR's
   `description`, and set the research doc's `links[]` back to the FR with
   `relationship: informs` (the field is required). A contradiction between inputs is
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
   - A **RESEARCH** doc has no `meta.id`/`surface`/`owner`/`priority`, but it DOES take a
     typed storage id: `RESEARCH-<SURFACE>-<SLUG>` (the store accepts `--id` on any
     doc-type, and the cmd research set already uses this form). Pass it on create so the
     FR's description and the prediction fingerprint can name something readable.
   - **Create order**: NFRs first, then the FRs that reference them, then the RESEARCH
     documents whose `links[]` point at FRs. The store checks neither reference existence
     nor `depends_on` cycles on create, so a wrong order lands silently; the order is your
     only check.
   - Write the JSON files with a Bash heredoc; the skill's tools are Bash and Read.

   ```
   legion document validate --schema <requirement-schema-id> --file fr-sample.json
   legion document validate --schema <nfr-schema-id> --file nfr-sample.json
   legion document validate --schema <research-schema-id> --file research-sample.json
   legion document create --doc-type requirement --id FR-<SURFACE>-001 --priority SHALL --owner <agent> --surface <surface> --from <file>
   legion document create --doc-type nfr --id NFR-<SURFACE>-001 --owner <agent> --surface <surface> --from <file>
   legion document create --doc-type research --id RESEARCH-<SURFACE>-<SLUG> --owner <agent> --surface <surface> --from <file>
   ```

6. **Report** the set: the FR and NFR ids, the RESEARCH documents spun off, every gap
   escalated with the signal sent for it, and the overlap with any pre-existing spec on a
   sibling surface. One scope's spec per invocation. When you run as a spawned agent,
   deliver the report with SendMessage and end your turn with one line, so the harness's
   idle notice does not repeat a truncated copy of it.

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
