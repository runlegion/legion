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
   it doubts a mechanism -- and may split into one of each. Each entry also carries a
   `key`: the intent's `claims[].id` when the entry came from a test card, otherwise the
   entry's 1-based position in `claims_to_test`. The key is what the prediction
   fingerprint is built on (Instrumentation below), and sd-discover reads it from the
   agenda to witness the claim, so it never changes once the agenda is handed on.

4. **Emit the predictions** (see Instrumentation below): one per `claims_to_test` entry,
   after the agenda is final so every fingerprint names a key sd-discover will read.
   Write each returned prediction id into its entry as `prediction`; the agenda is the
   handoff, and the witness needs the id.

5. Report the agenda to the caller as structured text (services_to_test and
   claims_to_test, each item with its intent trace, its key, its prediction id, and the
   claimed confidence). This step writes no legion document;
   the agenda is working state, and handing it to the next step as a scratch FILE is fine
   -- a file is not a document, and the prohibition is on store writes, not on writing the
   agenda down. If the session must stop here, park per the protocol in the
   sd-service-design skill (Park and resume) with the agenda, prediction ids included, in
   the anchor text.

## Instrumentation

This step's one judgment is which claims the world will bear out. Deriving the agenda is
not a prediction -- every entry traces to an intent field by rule -- so the agenda itself
gets no emission. Each claim to test does: one prediction per entry, under this skill's
name, that sd-discover will return it `supported` or `bounded`. The number is what makes
the review accountable for the intent's optimism instead of a relay of it. The prediction
row is the engine's, not a document; the rule against store writes here is about
artifacts, and a prediction is not one.

Stake each claim from the intent's own evidence fields, not from how much the intent
wants it. Anchors: a claim from a `claims[]` test card whose `right_if` names a query a
lens already in `evidence.lenses` can answer sits near 0.7; a claim implied by a
`cut_or_broken` entry's `why`, with a lens, sits near 0.5; a claim whose only lens is the
CRAWL lens-to-be, or one raised by an open question that doubts the need, starts near
0.3. From the anchor, move by what the intent already cites: a `meta.sources` entry of
kind `audit` or `reflection` that reports the pain firsthand moves the number up; a
`known_gaps` entry admitting the pain has not been observed moves it down. Different
claims get different numbers; an agenda emitted at one value teaches the estimator
nothing about which kinds of claim the intent gets wrong.

```
legion uncertainty emit --surface legion.sd --feature-key sd.intent-review.claim \
  --session-id "$CLAUDE_CODE_SESSION_ID" --orphan-ttl-days 180 \
  --input-fingerprint <intent-id>:claim:<key> --claimed-confidence <p> \
  --payload '{"intent":"<intent-id>","key":"<key>","from":"<intent field>","lens":"<lens>"}'
```

The emit mechanics -- session id and model, the exit-0 rule, the 180-day orphan window,
non-blocking emission, never self-witnessing -- are held once in the sd-service-design
skill (Instrumentation, "Emit mechanics") and bind here. What is this step's alone: the
check is each fingerprint against the agenda entry it names, and since no document
lands, the agenda file and the report carry the ids.

**Who witnesses, and when.** sd-discover, at its authoritative scoring pass, when it
lands a verdict on the claim. It reads the entry's `prediction` id from the agenda,
confirms it by rebuilding `<intent-id>:claim:<key>`, and witnesses by the verdict:
`supported` is `shipped` at 1.0, `bounded` is `scoped-down` at 1.0 (support within
limits is still support), `contradicted` is `abandoned` at 0.0. A `blocked` insight
leaves the prediction unwitnessed: no verdict was reached, the wake may still reach one,
and a corpus that never does lets the orphan sweep retire it, which is right for a claim
never tested. A `saturated-unevidenced` insight also leaves it unwitnessed: silence
argues against the claim without being discourse under the bar, and scoring it 0.0 would
teach the estimator that silence is contradiction, the confusion the no-evidence rule
exists to prevent. The discover report names each claim left unwitnessed and why.

## Refuses

- Designing services: any output field beyond `{name, actor, goal, test}` per service.
- Stamping build status: real versus planned is build state and belongs to later steps.
- Inventing claims the intent neither states nor implies -- an emergent insight is
  sd-discover's to discover from evidence, not this step's to guess.
- Reviewing an intent that does not exist or does not validate: stop and say so.
- Witnessing its own predictions. The review stakes them; sd-discover's verdicts score
  them. A self-witnessed claim is the rubber stamp the engine exists to catch.
