---
name: sd-thesis-review
description: |
  The first service-design step: read a repo's thesis document and produce a research
  agenda -- candidate services to test and pains to prove -- with every item tracing to a
  thesis field. Outputs hypotheses to validate against real discourse, never designed
  services. Invoke at the start of a repo's service design, before any artifact exists.
version: 0.1.0
user-invocable: true
allowed-tools: Bash, Read
---

# Thesis review: thesis in, research agenda out

This step steps BACK from the thesis, not forward from it. Its output is what to go ask
the world about -- not services, not artifacts, not statuses. The one failure mode that
matters: over-producing. A reviewed thesis that comes back with system responses, real
versus planned stamps, or merged service definitions has jumped two steps ahead; that is
service design, and it happens only after the pains are proven.

## Procedure

1. Read the thesis in full: `legion document view <thesis-id> --json`. The fields that
   feed the agenda: `what_it_is` (the reason to exist), `direction.becoming` and its
   proposals, `current_state.cut_or_broken` (each entry's `why` is a pain claim),
   `current_state.known_gaps` where present, `open_questions` (unresolved ones ARE agenda
   items), and `evidence` (existing lenses and crawl topics constrain where proof can come
   from).

2. Derive **services_to_test**: for each service the thesis implies, one entry
   `{name, actor, goal, test}` where `test` states what real discourse would confirm the
   need exists. Lightweight hypotheses only -- no `system_response`, no real/planned
   status, no merging or splitting of services. If two candidate services blur together,
   list both; pain-listen's evidence will sort them.

3. Derive **pains_to_prove**: for each pain the thesis asserts or implies, one entry
   `{pain, who, evidence_target, disconfirm_criterion}` where `evidence_target` names the
   lens (or lens-to-be) and the query that would surface it, and `disconfirm_criterion`
   states what result kills it (the standing rule downstream: an on-topic score under 0.40
   is a kill). Every entry names the thesis field it came from.

4. Report the agenda to the caller as structured text (services_to_test and
   pains_to_prove, each item with its thesis trace). This step writes no document; the
   agenda is working state. If the session must stop here, park per the sd-service-design
   protocol with the agenda in the anchor text.

## Refuses

- Designing services: any output field beyond `{name, actor, goal, test}` per service.
- Stamping build status: real versus planned is build state and belongs to later steps.
- Inventing pains the thesis neither states nor implies -- an emergent pain is
  pain-listen's to discover from evidence, not this step's to guess.
- Reviewing a thesis that does not exist or does not validate: stop and say so.
