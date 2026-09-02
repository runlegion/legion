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

## Two invariants (bind every requirement)

- **Every SHALL names what earned it.** A requirement's `traces_to` cites the source that
  makes it true: a proven toy or experiment, a supported or bounded insight (by id), a
  settled intent proposal, an operator ruling on a resolved open question, a settled
  boundary rule, an actor's core stake, an ecosystem moment of truth. The list is
  exhaustive: a mapping rule that would grant a SHALL on anything else is a defect in one
  of the two, to reconcile rather than to follow. A candidate SHALL whose only ground is
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
landed artifact is invariant 2: land the set, name the gap, escalate. A weak artifact
(a Discovery whose insights are all `blocked`) is landed, not missing; the run proceeds
and the verdicts decide what earns what.

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
  corpus with reproductions, a probe script that found real defects) IS invariant 1's "proven
  toy or experiment" and earns a SHALL; a `real` entry that merely describes the present
  system earns nothing. A `known_gap` is a pain the requirement addresses, not a
  requirement itself; it may ground an NFR's `traces_to` alongside the actor's stake that
  says the gap must be closed, never alone.
- `boundaries` -- an entry whose `note` states a rule the scope must hold to ("must never
  depend on X", "the dependency runs one way") is a settled constraint and earns a SHALL,
  folded into an FR since no constraint doc-type exists. An entry that only assigns
  ownership is scoping, not a requirement: when it hands a capability to another owner,
  that capability is out of this scope, and a requirement that would cross the line is the
  boundary violation to name and escalate rather than spec.
- `open_questions` -- an unresolved one, or any unresolved contradiction, is escalated,
  not resolved (invariant 2). A resolved one whose `resolution` records an operator
  ruling earns a SHALL for the outcome it names. A field elsewhere in the intent that
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
- **FRs** from blueprint `steps[].layers` (`backstage`, `support`) -- these say what the
  system must do, so they are where a requirement is found, never what makes it true. A
  blueprint step is a supporting token, never the earner: the earner is the supported
  insight, moment of truth, or settled proposal the step serves. A step with none of
  those behind it earns no SHALL; it takes a SHOULD from the journey goal it serves, per
  the priority rule below, and where it serves no goal either it is escalated. From the
  ecosystem's `moments_of_truth`: a moment of truth's
  `success` text earns a SHALL; its `failure` text is what the `errors` object guards
  against and earns nothing on its own. A persona's own `moment_of_truth` supports a
  citation; only the ecosystem's earn.
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
- **NFRs** from moments of truth and non-functional concerns. A `known_gap` is evidence of
  the pain, not an earner: cite it beside the stake that earns the NFR, and where no stake
  says the gap must be closed, escalate it rather than writing a SHALL on the gap alone.
  In intent-only mode, NFRs come from `actors` stakes and the source notes in
  `meta.sources` (a cited
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
`boundary.<owner>`. The first token is the earner; the rest support. Only a token naming
something invariant 1 lists may lead: `intent.<json-path>` for a settled proposal, a
ruling, an actor's stake, or a proven experiment; `boundary.<owner>` for a settled
boundary rule; `discovery.<insight-id>` for a supported or bounded insight;
`moment_of_truth.<n>`. `blueprint.step.<n>`,
`journey.phase.<n>`, and `persona.<slug>.needs[<i>]` are supporting tokens only.
`meta.priority` holds the value; `traces_to` shows how it was derived. That is one
source, shown twice, not two sources.

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

6. **Emit the predictions** (see Instrumentation below): one for the set, one per RESEARCH
   document. Emit after the documents land, so every prediction names a real id.
7. **Report** the set: the FR and NFR ids, the RESEARCH documents spun off, every gap
   escalated with the signal sent for it, the overlap with any pre-existing spec on a
   sibling surface, and every prediction id emitted with its claimed confidence. One
   scope's spec per invocation. When you run as a spawned agent, deliver the report with
   SendMessage and end your turn with one line, so the harness's idle notice does not
   repeat a truncated copy of it.

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
