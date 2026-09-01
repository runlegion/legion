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

- **Thesis-only** (system work -- legion-cmd is the case): the intent alone. Requirements
  derive from the intent's `claims` (each claim's `right_if` becomes acceptance criteria),
  `direction.proposals` (settled ones become SHALLs, `needs_pressure_test` ones route to
  RESEARCH), and `current_state` (real vs planned governs tense, never priority).
- **Service-design** (product work): the intent plus the landed ecosystem, personas,
  journeys, blueprints, and Discovery. Read every input by id first
  (`legion document view <id> --json`); the requirement set is derived from all of them,
  and each requirement traces to the specific source it came from.

There is no story layer. A requirement traces DIRECTLY to its source in the intent and the
ecosystem (decided 2026-06; reconfirmed) -- the intent's claims and the ecosystem's moments
of truth carry what a user-story layer would have, so `traces_to` points straight at them.

## The mapping

- **FRs** from blueprint `backstage`/`support` mechanisms and the intent's claims -- what
  the system must do.
- **Error-handling requirements** from blueprint `fail_points` (where the service breaks)
  and the ecosystem's `failure_modes` -- what must happen when it does. These populate the
  requirement's `errors` object.
- **NFRs** from moments of truth and non-functional concerns -- performance, security,
  reliability, and kin. Each NFR needs `category` (a closed enum:
  performance/scalability/reliability/availability/security/privacy/observability/maintainability/compatibility/usability/compliance
  -- use `scalability` for locality, `maintainability` for policy-as-data), `metric`,
  `target`, and `measurement`; an NFR without a measurement is an aspiration, not an NFR.
- **Priority is derived at the spec boundary and shown in `traces_to`** -- designers do not
  speak in absolutes, so the source artifacts carry none; this skill assigns them and shows
  its work: a persona's `would_leave_if` backed by a supported insight -> **SHALL**; a
  `goal` the service serves -> **SHOULD**; a delighter-shaped opportunity -> **MAY**. The
  `traces_to` string states the derivation, so a reviewer can attack it.

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
   `NFR-<SURFACE>-NNN`. Set `depends_on` between requirements, `nfr_refs` from an FR to the
   NFRs that bound it, and `constraint_refs` where a constraint governs -- the cross-links
   only cohere if the set is written together, which is why this is one invocation, not one
   per requirement.
4. **Route unproven ground to RESEARCH, escalate UNCLEAR.** A claim the inputs cannot
   ground becomes a RESEARCH document (its own doc-type) that the requirement cites, not a
   SHALL. A contradiction between inputs is named in the requirement and signalled up, not
   silently resolved.
5. **Validate one FR and one NFR before batch-creating.** The store refuses a schema
   violation on every path; catching it on one document is cheaper than on the whole set:

   ```
   legion document validate --schema <requirement-schema-id> --file fr-sample.json
   legion document validate --schema <nfr-schema-id> --file nfr-sample.json
   legion document create --doc-type requirement --owner <agent> --surface <surface> --from <file>
   legion document create --doc-type nfr --owner <agent> --surface <surface> --from <file>
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
- Resolving an input contradiction in-body. It escalates.
- Any status beyond `draft`. Acceptance is the crit, a separate step.
