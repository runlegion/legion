---
name: sd-intent-review
description: |
  The first service-design step: read a repo's intent document and produce a research
  agenda -- candidate services to test and claims to test -- with every item tracing to an
  intent field. Outputs hypotheses to validate against real discourse, never designed
  services. Invoke at the start of a repo's service design, before any artifact exists.
version: 0.1.0
user-invocable: true
allowed-tools: Bash, Read
---

# Intent review: intent in, research agenda out

This step steps BACK from the intent, not forward from it. Its output is what to go ask
the world about -- not services, not artifacts, not statuses. The one failure mode that
matters: over-producing. A reviewed intent that comes back with system responses, real
versus planned stamps, or merged service definitions has jumped two steps ahead; that is
service design, and it happens only after the claims are tested.

## Procedure

1. Read the intent in full: `legion document view <intent-id> --json`. The fields that
   feed the agenda: `what_it_is` (the reason to exist), `direction.becoming` and its
   proposals, `current_state.cut_or_broken` (each entry's `why` is a claim),
   `current_state.known_gaps` where present, `open_questions` (unresolved ones ARE agenda
   items), and `evidence` (existing lenses and crawl topics constrain where proof can come
   from).

2. Derive **services_to_test**: for each service the intent implies, one entry
   `{name, actor, goal, test}` where `test` states what real discourse would confirm the
   need exists. Lightweight hypotheses only -- no `system_response`, no real/planned
   status, no merging or splitting of services. If two candidate services blur together,
   list both; sd-discover's evidence will sort them. A service with two actors names the
   primary in `actor` and the second inside `goal`.

3. Derive **claims_to_test**: for each claim the intent asserts or implies (start from its `claims[]` test cards where present), one entry
   `{claim, who, evidence_target, right_if}` where `evidence_target` names the
   lens (or lens-to-be) and the query that would surface it, and `right_if`
   states what result confirms or kills it (the standing rule downstream: an on-topic score under 0.40
   is a contradiction). Every entry names the intent field it came from. When the only lens is the
   intent's `crawl_topic` (`needs_crawl` true), label that lens-to-be CRAWL and define the
   mapping once at the top of the agenda so downstream steps can match on it. An
   unresolved open_question lands as a claim when it doubts a need, as a service test when
   it doubts a mechanism -- and may split into one of each.

4. Report the agenda to the caller as structured text (services_to_test and
   claims_to_test, each item with its intent trace). This step writes no legion document;
   the agenda is working state, and handing it to the next step as a scratch FILE is fine
   -- a file is not a document, and the prohibition is on store writes, not on writing the
   agenda down. If the session must stop here, park per the protocol in the
   sd-service-design skill (Park and resume) with the agenda in the anchor text.

## Refuses

- Designing services: any output field beyond `{name, actor, goal, test}` per service.
- Stamping build status: real versus planned is build state and belongs to later steps.
- Inventing claims the intent neither states nor implies -- an emergent insight is
  sd-discover's to discover from evidence, not this step's to guess.
- Reviewing an intent that does not exist or does not validate: stop and say so.
