---
name: sd-service-design
description: |
  Run a repo's service design end to end: thesis-review -> pain-listen -> ecosystem-imagine ->
  the persona, journey, and blueprint writers. This skill is the orchestrator, written as
  instructions -- the agent that invokes it conducts the pipeline by following it. Each step
  reads and writes documents in the legion store by id; a validation gate sits between steps;
  a step that cannot finish parks and resumes instead of guessing. Invoke when a repo starts
  or resumes its service design.
version: 0.1.0
user-invocable: true
allowed-tools: Bash, Read
---

# Service design, conducted

Service design here is discovery, not decoration: you hypothesize services, prove or kill
the pains underneath them against real discourse, and only then draw the artifacts. The
craft rules below bind every step; the step skills carry the mechanics.

## The craft rules

- **Disconfirmable first.** Nothing is asserted that has not been stated as a hypothesis
  with a disconfirm criterion. The thesis review produces hypotheses; pain-listen proves or
  kills them; artifacts are drawn only from what survived.
- **Evidence lives in the painmatrix.** Every claim in a persona, journey, or blueprint
  traces to a painmatrix theme or a thesis field. No artifact carries evidence of its own.
- **Emotions are data.** The schemas carry affect as valence numbers (-3 to 3 on journey
  phases and blueprint steps) plus plain emotion words; both come from what the evidence
  shows, and the number never carries more precision than the evidence supports. The word
  vocabulary is unconstrained -- no taxonomy has converged (T6 is an open design question,
  awaiting critique and evidence); never invent one.
- **No vaporware.** Nothing is described as existing unless the thesis's
  `current_state.real` says it exists. Planned things are named as planned.
- **A killed pain stays killed.** The painmatrix carries each theme's `status`: killed
  means disconfirmed by evidence (on-topic discourse under the `meta.threshold`
  disconfirm bar), and downstream artifacts must not smuggle a killed theme back in as a
  frustration or a journey low point. A `blocked` theme is neither proven nor killed,
  and no artifact may build on it; `proven` and `bounded` themes are buildable, a
  bounded one only within its recorded limits.

## The pipeline

Run the steps in order. Each step's output is a document in the legion store; carry
document IDS forward between steps, never re-derived prose. The repo's thesis document is
the root input -- find it with
`legion document list --doc-type thesis --json` and match the repo (a repo with no thesis
stops here: the thesis comes first, and writing one is not this pipeline's job).

1. **`sd-thesis-review`** -- thesis in, research agenda out (services to test, pains to
   prove). No documents written; the agenda is this session's working state and is stored
   in the park anchor if you stop.
2. **`sd-pain-listen`** -- agenda in, one schema-valid Painmatrix document out. This is the
   step that talks to the world (eavesdrop) and the step most likely to park. Expect it to
   take about a day: a fresh corpus supports only an orientation pass, and the
   authoritative scoring runs on the park wake after the crawl has accumulated.
3. **`sd-ecosystem-imagine`** -- thesis + painmatrix in, one Ecosystem document out, plus a
   register of flagged unknowns.
4. **The writers, in dependency order per chain**: `sd-write-persona` first (its actor
   must exist in the landed ecosystem), then `sd-write-journey` for that persona (it
   consumes the persona document by id), then `sd-write-blueprint` for that journey (its
   steps follow that journey's phases). A journey cannot be written before its persona,
   nor a blueprint before its journey -- each writer refuses a missing input document.
   Only separate chains parallelize: persona A's journey can be written while persona B is
   still being drafted, never ahead of persona A itself.

**The gate between steps:** before starting step N+1, confirm step N's output document
exists and validates -- `legion document view <id>` and
`legion document validate --schema <schema-id> --file <payload>` (resolve the schema id by
its `x-doc-type`; see the step skills for the exact lookup). The store refuses invalid
writes anyway; the gate exists so a step never starts from a half-landed input.

## Park and resume

A step that cannot finish -- a missing corpus, an unanswered operator question, evidence
that contradicts the thesis -- parks rather than guesses:

1. Write the step's output document as far as it got, status `draft`, with the blocked
   items named inside it (a painmatrix with three proven themes and two marked
   `blocked: crawl in flight` is a valid draft).
2. Store a checkpoint reflection naming the exact resume point:
   `legion reflect --repo <repo> --domain checkpoint --text "[SD ANCHOR] resume <skill> at <items>; waiting on <what>; inputs: <document ids>"`.
3. Arm a wake, one or both of:
   - timed: `legion defer --work-item sd-<repo>-<step> --repo <repo> --until 1d --note "<what you are waiting for>"`
   - event: `legion signal` to the agent that owes you the input, so their reply wakes you.
   Both end as a wake-worthy routing signal from the watch daemon; whichever fires first
   resumes the work.

On wake: recall the anchor (`legion recall --repo <repo> --domain checkpoint --limit 1`),
re-check the thing you were waiting on, and continue from the named items. The draft
document is the state; nothing lives only in a dead session's context.

## When the thesis revises after artifacts land

The root input is a living document; a revision after downstream artifacts exist
propagates by what actually changed, not by re-running the pipeline:

- **Painmatrix**: untouched by direction or proposal deltas -- only a changed PAIN CLAIM
  reopens listening. Nothing proven or killed moves because the plans did.
- **Ecosystem**: revises. This is the register loop closing: entries the flagged-unknowns
  register carried that the revision answers get recorded as resolved, and the value
  exchanges those answers ground get adjusted.
- **Blueprints**: relabel `PLANNED` to real only for what the revision marks settled AND
  actually shipping. Newly proposed items stay planned or absent -- no vaporware enters
  through a thesis bump.
- **Personas and journeys**: move only if a pain theme moved. Their traces are themes,
  not thesis prose.

## What this skill refuses

- Running steps out of order, or starting a step whose input document does not validate.
- Producing any artifact for a repo with no thesis.
- Dispatching the ecosystem register anywhere (where the register routes is unconverged
  design, T7): the register lives in the ecosystem document and in your report to whoever
  invoked you.

## Instrumentation

Before each step, emit one prediction; after it, the outcome is witnessed by whether the
step's document landed without a schema refusal:
`legion uncertainty emit --surface legion.sd --feature-key sd.<step> --input-fingerprint <repo>-<step> --claimed-confidence 0.7 --payload '{"repo":"<repo>","step":"<step>"}'`.
This is one line per step, not a framework; skip nothing.
