---
name: sd-service-design
description: |
  Run a repo's service design end to end: intent-review -> discover -> ecosystem-imagine ->
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

Service design here is discovery, not decoration: you state claims, and the listening returns them as insights with verdicts, and only then draw the artifacts. The
craft rules below bind every step; the step skills carry the mechanics.

## The craft rules

- **Disconfirmable first.** Nothing is asserted that has not been stated as a hypothesis
  with a disconfirm criterion. The intent review produces claims; sd-discover returns their verdicts; artifacts are drawn only from what survived.
- **Evidence lives in the Discovery.** Every statement in a persona, journey, or blueprint
  traces to a Discovery insight or an intent field. No artifact carries evidence of its own.
- **Emotions are data.** The schemas carry affect as valence numbers (-3 to 3 on journey
  phases and blueprint steps) plus plain emotion words; both come from what the evidence
  shows, and the number never carries more precision than the evidence supports. The word
  vocabulary is unconstrained -- no taxonomy has converged (T6 is an open design question,
  awaiting critique and evidence); never invent one.
- **No vaporware.** Nothing is described as existing unless the intent's
  `current_state.real` says it exists. Planned things are named as planned.
- **A contradicted insight stays contradicted.** The Discovery carries each insight's
  `status` verdict: contradicted means the evidence killed the claim (on-topic discourse
  under the `meta.threshold` disconfirm bar), and downstream artifacts must not smuggle
  it back in as a frustration or a journey low point. `blocked` (the corpus cannot reach
  it yet) and `saturated-unevidenced` (the listening converged on silence -- itself
  claim-disproving) are distinct fates: neither is buildable, but the second argues
  against the claim. `supported` and `bounded` insights are buildable, a bounded one only
  within its recorded limits.

## The pipeline

Run the steps in order. Each step's output is a document in the legion store; carry
document IDS forward between steps, never re-derived prose, and with them the prediction
ids each step reports (Instrumentation, "What the conductor carries and witnesses"). The
repo's intent document is the root input -- find it with
`legion document list --doc-type intent --json` and match the repo (a repo with no intent
stops here: the intent comes first, and writing one is not this pipeline's job).

1. **`sd-intent-review`** -- intent in, research agenda out (services to test, claims to
   test). No documents written; the agenda is this session's working state and is stored
   in the park anchor if you stop.
2. **`sd-discover`** -- agenda in, one schema-valid Discovery document out. This is the
   step that talks to the world (eavesdrop) and the step most likely to park. Expect it to
   take about a day: a fresh corpus supports only an orientation pass, and the
   authoritative scoring runs on the park wake after the crawl has accumulated.
3. **`sd-ecosystem-imagine`** -- intent + discovery in, one Ecosystem document out, plus a
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
that contradicts the intent -- parks rather than guesses:

1. Write the step's output document as far as it got, status `draft`, with the blocked
   items named inside it (a Discovery with three supported insights and two marked
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

## When the intent revises after artifacts land

The root input is a living document; a revision after downstream artifacts exist
propagates by what actually changed, not by re-running the pipeline:

- **Discovery**: untouched by direction or proposal deltas -- only a changed CLAIM
  reopens listening. No verdict moves because the plans did. The re-listen you dispatch
  witnesses the earlier verdict predictions it re-scores (Instrumentation below).
- **Ecosystem**: revises. This is the register loop closing: entries the flagged-unknowns
  register carried that the revision answers get recorded as resolved, and the value
  exchanges those answers ground get adjusted. Each resolved entry's prediction is
  witnessed here, by you (Instrumentation below).
- **Blueprints**: relabel `PLANNED` to real only for what the revision marks settled AND
  actually shipping. Newly proposed items stay planned or absent -- no vaporware enters
  through an intent bump.
- **Personas and journeys**: move only if an insight moved. Their traces are insights,
  not intent prose.

## What this skill refuses

- Running steps out of order, or starting a step whose input document does not validate.
- Producing any artifact for a repo with no intent.
- Dispatching the ecosystem register anywhere (where the register routes is unconverged
  design, T7): the register lives in the ecosystem document and in your report to whoever
  invoked you.

## Instrumentation

The judgment predictions live in the step skills, each under its own Instrumentation:
a claim's support, a verdict holding, an edge materializing, a document surviving the
crit, each with its named witness. The conductor's own emission is only the
step-completion claim -- that the step lands its output through the gate on this
invocation, without parking -- and its confidence is read from the step's inputs, never
a constant: sd-discover on a fresh corpus with `needs_crawl` true sits near 0.3, since it
parks by design; a writer whose inputs validate and whose insights are supported sits
near 0.9; an ecosystem pass over a Discovery with blocked insights sits near 0.6. Before
each step, one line:

```
legion uncertainty emit --surface legion.sd --feature-key sd.<step> \
  --session-id "$CLAUDE_CODE_SESSION_ID" --orphan-ttl-days 180 --input-fingerprint <repo>-<step> \
  --claimed-confidence <p> --payload '{"repo":"<repo>","step":"<step>"}'
```

How it resolves: by the gate, which is a store-readable fact rather than a judgment --
the step's document landed, parked as a draft, or was refused. The conductor does NOT
witness it. Staking a number and then scoring it is the rubber stamp the engine exists to
catch, and the confidence here is a judgment (read from the step's inputs) even though
the outcome is not. The step-completion claim is the natural first case for
store-resolved witnessing (#1091); until that lands it sits unwitnessed like every other
prediction in this pipeline, which is honest. Skip the emission for no step.

### What the conductor carries and witnesses

The step skills name the conductor in three places. Each rule lives in the step skill
that defines it; the conductor's job is to carry and to run, never to re-derive.

- **Carry prediction ids forward.** The agenda's per-claim `key` and `prediction` ids
  travel to sd-discover with the agenda (sd-intent-review, Instrumentation). The
  ecosystem report's register, with each entry's prediction id and route, travels to
  every sd-write-blueprint invocation in that ecosystem's chains (sd-ecosystem-imagine,
  "Who witnesses"): the blueprint writer witnesses world-routed edges from it, no
  document carries it, and a report you drop orphans those predictions. A park anchor
  carries both when a session stops.
- **Witness operator-routed register edges** at the ecosystem revision that records them
  resolved (the section above): answered as real is `shipped` at 1.0, answered as not a
  thing is `abandoned` at 0.0 (sd-ecosystem-imagine, "Who witnesses").
- **Witness insight verdicts when a claim reopens.** A changed claim reopens listening;
  the re-listen pass you dispatch witnesses the earlier Discovery's verdict predictions
  for the insights it re-scores, under sd-discover's rule ("Who witnesses a verdict"),
  and is never the pass that emitted them.

### Emit mechanics

These rules bind every emit in the pipeline, the conductor's and each step's. The step
skills cross-reference this block instead of restating it, as they do the park protocol.

- Omit `--model` and pass `--session-id` from the `CLAUDE_CODE_SESSION_ID` environment
  variable: the engine resolves the model from that session's live statusline sample,
  and with neither flag the row lands in the `unknown` model cohort, where no regression
  across releases can ever be seen. A guessed `--model` is worse: it mislabels the row
  into a real cohort. If the variable is unset in your shell, emit anyway and say so in
  the report; the rows will sit in `unknown`.
- Emit exits 0 even when it recorded something you did not mean (a wrong id in the
  fingerprint still returns a valid-looking id), and the engine has no read-back
  command: `witness` takes only the id emit printed. The only check is the command line
  you ran against the create output; do it before you report.
- The default orphan window is 30 days, and neither a crit, a research toy, nor a
  re-listen reliably happens inside it, so every emit in this pipeline sets
  `--orphan-ttl-days 180`. A prediction that still orphans after that is a finding about
  the pipeline, not an error in the emit.
- Record each prediction id in the report next to the thing it is about, with whatever
  the fingerprint was built from, so the witness can rebuild the string to confirm the
  id. Do not revise a landed document to hold the id; the report, the park anchor, and
  the fingerprint are how the witness finds it.
- Emission is non-blocking by design: a failed emit logs and exits 0, and the run
  continues. A step that lands its output and emits nothing has skipped a step; say so
  in the report.
- Never witness your own prediction, without exception. The emitter stakes it; the named
  witness scores it. A self-witnessed prediction is the rubber stamp the engine exists to
  catch, and a mechanical-looking outcome does not earn a carve-out while the confidence
  staked against it is still a judgment.
