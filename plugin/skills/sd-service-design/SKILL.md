---
name: sd-service-design
description: |
  Run a repo's service design through the first diamond -- discover the problem, then define
  the service: intent-review -> discover -> ecosystem-imagine -> the persona, journey, and
  blueprint writers. It ends at a defined service, not a solution, a spec, or code. This
  skill is the conductor: it dispatches each step as a plugin agent with the Agent tool,
  passes document ids, gates between steps, and parks and resumes when a step returns
  parked. Invoke when a repo starts or resumes its service design.
version: 0.3.0
user-invocable: true
allowed-tools: Bash, Read, Agent
---

# Service design, conducted

Each step of the pipeline is a plugin agent with a strict brief: it creates one thing from the
inputs it is given, returns, and stops. The craft rules live in the agents. This skill holds
what spans the steps: the order, the gate, the park and resume, the intent-revision rules,
the prediction ids carried between steps, and the witnessing the conductor owns. It is a
skill, not an agent, because it dispatches the other steps and an agent cannot spawn agents.

## Where this sits

Legion's work runs three diamonds, each one spreading out and then narrowing:

1. **The problem.** Discover (intent-review, discover, eavesdrop asking real people) spreads
   out and takes as long as listening takes. Define (ecosystem, personas, journeys,
   blueprints) narrows to one defined service: who it is for, what makes it a product,
   where it wins, where it breaks.
2. **The solution design.** Develop explores and tests design solutions with people;
   Deliver produces the finished design assets and the design system.
3. **The engineering.** Research documents and toys explore how to build it; specs,
   issues, code, review, and verify narrow to the built system.

This skill runs the first diamond and nothing else. The second is designed separately.
`sd-write-spec` belongs to the third and has its own entry point; this pipeline never
dispatches it.

## Dispatching a step

Dispatch each step with the Agent tool, `subagent_type: "legion:<step>"`, and a prompt that
carries the input document ids and nothing else: no restated rules, no added cases, no
summary of earlier steps. The agent reads its inputs from the store. Three inputs are not
document ids and still ride in the prompt: the research agenda, passed to sd-discover as the
file path sd-intent-review returned; the prediction ids a re-listen pass witnesses; and the
actor's name, passed to sd-write-persona.

Every agent returns one block, in one of three forms:

- `done`: the created document ids, each prediction id with its fingerprint and claimed
  confidence, and, for sd-intent-review, the agenda.
- `parked`: the document id if one was written (at `draft`, or `review` where the step lands
  there for operator rulings), the predictions already emitted, what the step waits on, and
  any question for real people or the operator, with a recommended answer and its reasoning.
- `stopped`: the input gaps that kept the step from starting.

A `stopped` return is a gate failure: fix the input (or take the intent's distill gaps back
to its writer) before dispatching again. A `parked` return goes to Park and resume.

## The pipeline

Run the steps in order. Each step's output is a document in the legion store; carry document
ids forward between steps, never re-derived prose, and with them the prediction ids each step
returns. The repo's intent document is the root input -- find it with
`legion document list --doc-type intent --json` and match the repo (a repo with no intent
stops here: the intent comes first, and writing one is not this pipeline's job).

1. **`sd-intent-review`** -- dispatch with the intent id. It returns the research agenda
   (services to test, claims to test with keys and prediction ids, and escalations) and the
   path of the agenda file. No store document is written; the agenda is this session's
   working state. When the agenda carries escalations (UNDECIDED proposals), park for the
   operator's rulings before sd-discover runs, and re-dispatch the review on the revised
   intent.
2. **`sd-discover`** -- dispatch with the intent id and the agenda path, and on each wake with
   the draft Discovery id too. The step most likely to park: a fresh corpus supports only an
   orientation pass, and the authoritative scoring runs on the wake after the crawl has
   accumulated, about a day later. A Discovery that returns at `review` carries contradicted
   or saturated-unevidenced insights, or challenge notes on committed items: take each to the
   operator with its recommended ruling, and wait for the rulings before step 3. When every
   ruling is in, record them by revising the Discovery to `done` (`legion document revise`);
   the intent's owner makes any change to the intent. sd-ecosystem-imagine returns `stopped`
   on a Discovery still at `review`.
3. **`sd-ecosystem-imagine`** -- dispatch with the intent and Discovery ids, and on a redraw
   with the ecosystem id too. It may park on questions for real people or choices for the
   operator; keep its register (each entry's prediction id and route) until every entry is
   answered.
4. **The writers, once the ecosystem's register is empty** -- every entry marked `ANSWERED:`
   and the ecosystem at `done`. Then, per chain, in dependency order:
   - `sd-write-persona` with the intent, Discovery, and Ecosystem ids and the actor's name
     (the actor must exist in the landed ecosystem). When it returns `done`, revise the
     ecosystem (`legion document revise`) so that actor's `actors.primary[].persona` names the
     landed persona id; that move is the conductor's alone.
   - `sd-write-journey` with that persona's id and the Discovery, Ecosystem, and intent ids.
   - `sd-write-blueprint` with that journey's id and the Discovery, Ecosystem, and intent ids.

   A journey waits for its persona, and a blueprint for its journey. Only separate chains
   parallelize: persona A's journey can be written while persona B is still being drafted,
   never ahead of persona A itself.

**The gate between steps:** before dispatching step N+1, confirm step N's output document
exists and validates -- `legion document view <id>` and
`legion document validate --schema <schema-id> --file <payload>`, resolving the schema id by
its `x-doc-type` from `legion document list --doc-type schema --json`. The store refuses
invalid writes anyway; the gate exists so a step never starts from a half-landed input.

## Park and resume

A step that cannot finish returns `parked` with its draft landed. The conductor parks it:

1. Store a checkpoint reflection naming the exact resume point:
   `legion reflect --repo <repo> --domain checkpoint --text "[SD ANCHOR] resume <step> at <items>; waiting on <what>; inputs: <document ids>; predictions: <ids>"`.
   The anchor carries the agenda path and every prediction id not yet in a landed document
   (the agenda's claim predictions, the ecosystem register's).
2. Arm a wake, one or both of:
   - timed: `legion defer --work-item sd-<repo>-<step> --repo <repo> --until 1d --note "<what you are waiting for>"`
   - event: `legion signal` to the agent that owes the input, so its reply wakes you.
3. Route what the return names:
   - **A crawl** (sd-discover on a missing or thin corpus): signal the eavesdrop agent naming
     the lens and why, and ask it to keep the lens warm (`eavesdrop daemon <lens> -i 6h`).
   - **Source depth** (a crawl slice that came back unusable): signal the eavesdrop agent,
     who owns the lens, with the sources that failed and the population the claims need.
   - **A question for real people**: signal the eavesdrop agent with the question and the
     lens. Answers arrive over days; re-arm the timed wake rather than giving up on them.
   - **An operator choice**: take it to the operator with the step's recommended answer and
     reasoning, so they can agree in one line.

A narrowing step whose evidence is too thin to decide goes back to listening and parks until
the answer comes; the operator gets only strategy and values choices.

On wake: recall the anchor (`legion recall --repo <repo> --domain checkpoint --limit 1`),
re-check the thing you were waiting on, and dispatch the step again with its inputs and the
draft document id. The draft document is the state; nothing lives only in a dead session's
context.

**sd-discover's two-pass cadence.** After an orientation pass returns `parked`, re-arm the
defer; never clear it. Only when the authoritative pass returns (at `done`, or `parked` at
`review` for rulings) clear it: `legion undefer --work-item <id>` (it takes no `--repo`). The
return's documents line names the pass (orientation, authoritative, or re-listen), so read it
there.

## When the intent revises after artifacts land

The root input is a living document; a revision after downstream artifacts exist
propagates by what actually changed, not by re-running the pipeline:

- **Discovery**: untouched by direction or proposal deltas -- only a changed CLAIM
  reopens listening. No verdict moves because the plans did. Dispatch sd-discover as the
  re-listen pass with the intent id, the agenda path, the landed Discovery id, and the
  prediction ids of the insights the changed claim touches; it witnesses those earlier
  verdicts, revises the Discovery, and is never the pass that emitted them.
- **Ecosystem**: revises. This is the register loop closing: dispatch sd-ecosystem-imagine
  with the ecosystem id, and open entries the revision answers are recorded as answered, and
  the answers they ground are adjusted. Each answered entry's prediction is witnessed by you
  (below).
- **Blueprints**: relabel `PLANNED` to real only for what the revision marks settled AND
  actually shipping. Newly proposed items stay planned or absent -- no vaporware enters
  through an intent bump.
- **Personas and journeys**: move only if an insight moved. Their traces are insights,
  not intent prose.

## What this skill refuses

- Running steps out of order, or dispatching a step whose input document does not validate.
- Producing any artifact for a repo with no intent.
- Handing the operator a question the world can answer, or a gap the step should have
  closed: route it back to listening.
- Dispatching `sd-write-spec`, or any step of the second or third diamond.
- Briefing an agent with anything beyond its inputs.

## Instrumentation

The judgment predictions live in the step agents: a claim's support, a verdict holding, a
drafted answer holding, a document surviving the crit, each with its named witness. The
conductor's own emission is only the step-completion claim -- that the step lands its output
through the gate on this dispatch, without parking -- and its confidence is read from the
step's inputs, never a constant: sd-discover on a fresh corpus with `needs_crawl` true sits
near 0.3, since it parks by design; a writer whose inputs validate and whose insights are
supported sits near 0.9; an ecosystem pass over a Discovery with blocked insights sits near
0.6. Before each dispatch, one line:

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

- **Carry prediction ids forward.** The agenda's per-claim `key` and `prediction` ids travel
  to sd-discover in the agenda file, which is its witness. The ecosystem register, with each
  entry's prediction id and route, stays with you until every entry is answered: no document
  carries it, and a return you drop orphans those predictions. A park anchor carries both
  when a session stops.
- **Witness register entries** when their answers land, confirming each id by rebuilding
  `<ecosystem-id>:edge:<n>` from the register:
  - entries sent to real people, when the redrawn ecosystem lands: an answer that confirms
    the drafted one is `shipped` at 1.0, one that changes it in part `scoped-down` at 0.5,
    one that overturns it `abandoned` at 0.0. The redraw itself never witnesses;
  - entries sent to the operator, at the ecosystem revision that records the ruling: the
    recommendation accepted is `shipped` at 1.0, accepted with changes `scoped-down` at 0.5,
    rejected `abandoned` at 0.0.

  An entry nobody answers orphans, the right fate for a question nobody took up.
- **Dispatch the re-listen when a claim reopens.** The re-listen pass witnesses the earlier
  Discovery's verdict predictions for the insights it re-scores. When no pass comes before
  the orphan window closes, the operator may witness by direct judgment against the same
  rule, or let it orphan.

### Writer predictions and the crit

Each writer stakes one prediction that the crit (the acceptance step that moves the document
past `draft`) accepts it as written; until the crit exists as a skill, the operator who moves
the document past `draft` witnesses by hand with the same rule, confirming the id by
rebuilding the fingerprint from the writer's return:

- persona, `<ecosystem-id>:persona:<persona-id>`: `outcome_correctness` is the fraction of
  statements (behaviors, goals, frustrations, `would_leave_if`) accepted as written;
- journey, `<persona-id>:journey:<journey-id>`: the fraction of phases accepted as written,
  affect and traces included;
- blueprint, `<journey-id>:blueprint:<blueprint-id>` (the pair lives in the return and the
  payload, since the blueprint's meta has no journey field): the fraction of steps accepted
  as written, backstage and frictions included.

The label is `shipped` when nothing was struck, added, or redrawn; `scoped-down` when the
crit cut statements, phases, or steps, flattened a dip, or moved a mechanism to planned;
`escalated` when it sent the document back.

### Emit mechanics

These rules bind every emit in the pipeline; each step agent restates them for its own.

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
- Record each prediction id next to the thing it is about, with whatever the fingerprint
  was built from, so the witness can rebuild the string to confirm the id. Do not revise a
  landed document to hold the id; the return, the park anchor, and the fingerprint are how
  the witness finds it.
- Emission is non-blocking by design: a failed emit logs and exits 0, and the run
  continues. A step that lands its output and emits nothing has skipped a step; say so.
- Never witness your own prediction, without exception. The emitter stakes it; the named
  witness scores it. A self-witnessed prediction is the rubber stamp the engine exists to
  catch, and a mechanical-looking outcome does not earn a carve-out while the confidence
  staked against it is still a judgment.
