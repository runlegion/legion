---
name: sd-service-design
description: |
  Run a repo's service design through the first diamond -- discover the problem, then define
  the service: intent-review -> discover -> per persona group the persona, journey, and
  blueprint writers -> ecosystem-imagine, written last from what the writers produced. It ends
  at a defined service, not a solution, a spec, or code. This skill is the conductor: it
  dispatches each step as a plugin agent with the Agent tool, passes document ids, gates
  between steps, and parks and resumes when a step returns parked. Invoke when a repo starts
  or resumes its service design.
version: 0.3.0
user-invocable: true
allowed-tools: Bash, Read, Agent
---

# Service design, conducted

Service design here is discovery, not decoration: the intent raises questions for people,
discovery answers them with cited first-hand evidence, and the artifacts are drawn from that
evidence. Each step after the intent review is a plugin agent with a strict brief: it creates
one thing from the inputs it is given, returns, and stops. The craft rules live in the agents.
This skill holds what spans the steps: the order, the gate, the park and resume, the
intent-revision rules, the prediction ids carried between steps, and the witnessing the
conductor owns.

## Where this sits

Legion's work runs three diamonds, each one spreading out and then narrowing:

1. **The problem.** Discover (intent-review, discover, eavesdrop asking real people) spreads
   out and takes as long as listening takes. Define (personas, journeys, blueprints, and
   last the ecosystem) narrows to one defined service: who it is for, what makes it a
   product, where it wins, where it breaks.
2. **The solution design.** Develop explores and tests design solutions with people;
   Deliver produces the finished design assets and the design system.
3. **The engineering.** Research documents and toys explore how to build it; specs,
   issues, code, review, and verify narrow to the built system.

This skill runs the first diamond alone. The second is designed separately. `sd-write-spec`
belongs to the third and has its own entry point outside this pipeline.

A step whose evidence is too thin to decide goes back to listening -- more crawl, or a
question eavesdrop asks real people -- and parks until the answer comes. The operator gets
only strategy and values choices.

## The craft rules

- **The operator's committed direction is a given; only the open parts are hypotheses.** A
  `settled` proposal, the `what_it_is` framing, and the intent's `boundaries` are the
  operator's bet -- Discovery informs HOW they are designed, and leaves WHETHER they are
  needed to the operator (an intent's `meta.purpose` may say so outright). A fully-committed
  intent is designed straight through.
- **Evidence lives in the Discovery.** Every statement in a persona, journey, blueprint, or
  ecosystem cites Discovery citation ids, a landed document, or an intent field. The
  artifacts carry no evidence of their own.
- **Emotions are data.** The schemas carry affect as valence numbers (-3 to 3 on journey
  phases and blueprint steps) plus plain emotion words; both come from what the stories
  say, and the number carries only the precision the evidence supports. The word vocabulary
  is open -- no taxonomy has converged (T6 is an open design question, awaiting critique
  and evidence).
- **What exists is drawn as existing; what is planned is marked planned.** A thing is
  described as existing only when the intent's `current_state.real` says it exists.
- **Gaps stay gaps.** An open question stays open, a contested answer carries both sides,
  and an item the evidence leaves empty stays empty in every artifact built from it, listed
  with the question to ask people.
- **The world answers what it can; the operator answers only what is theirs.** Every open
  question goes to whoever can answer it: the step's own design reasoning first, the
  Discovery's evidence next, then eavesdrop asking real people on the right lens, with
  the step parked until answers arrive. The operator gets only strategy and values
  choices -- what the product should be, what it will refuse to do -- and each arrives with
  a recommended answer and the reasoning behind it.
- **The customer may be an agent.** When an agent chooses and uses the product, the agent
  is the primary actor: it picks tools from help text, docs, and predictable output, and
  the moments of truth happen with it. The human behind it may never know the product
  exists, yet carries the risk and sees only outcomes. Design for both, on the assumption
  that the product has no way to ask the human anything. An agent is always an actor
  acting for someone (the intermediary or delegate pattern).

## The pipeline

Run the steps in order. Each step's output is a document in the legion store; carry
document ids forward between steps, and with them the prediction ids each step reports
(Instrumentation below). The repo's intent document is the root input -- find it with
`legion document list --doc-type intent --json` and match the repo. A repo with no intent
stops here: the intent comes first, and writing one is a separate job.

The agents read the service design primer by slug. Before step 2, confirm
`legion document view --slug sd-primer` resolves; it serves only an adopted reference, and
until the operator adopts it the pipeline stops after the intent review.

Dispatch each agent step with the Agent tool, `subagent_type: "legion:<step>"`, and a prompt
that carries the step's inputs by id and nothing else: the agent holds its own rules. Read
the return block at the end of its reply: `done` passes to the gate, `parked` goes to Park
and resume, `stopped` names what is missing.

1. **`sd-intent-review`** (skill) -- intent in, research agenda out (services to test,
   claims to test, and any escalations -- UNDECIDED proposals waiting on an operator choice).
   No documents written. When the agenda carries escalations, park for the operator's
   choices before discovery runs -- the direction is not yet fully classified.
2. **`legion:sd-discover`** -- the intent id and a working file path in, one Discovery out.
   Discovery raises its own questions from the intent's statements. This is the step that
   talks to the world, and the long one: a run that cannot finish in one session returns
   `parked` with the working file naming the next round, and you re-dispatch with the same
   path.
3. **The writers, per persona group.** For each persona group in the Discovery with stories
   from three or more independent authors, run one chain in dependency order:
   - **`legion:sd-write-persona`** -- the Discovery id and the group id in, one persona out.
   - **`legion:sd-write-journey`** -- that persona's id and the Discovery id in, one journey
     out.
   - **`legion:sd-write-blueprint`** -- that journey's id, the Discovery id, and the intent id
     in, one blueprint out.

   A journey follows its persona and a blueprint follows its journey; each writer returns
   `stopped` on a missing input document. Separate chains run in parallel. A group with
   fewer than three authors gets no chain; its empty items are open questions in the
   Discovery.
4. **`legion:sd-ecosystem-imagine`** -- last. The intent id, the Discovery id, and the ids of
   every landed persona, journey, and blueprint in, one Ecosystem out. It draws its actors,
   channels, value exchanges, moments of truth, and failure modes from those documents, and
   may park on questions for real people or operator choices.

**The gate between steps:** before starting a step, confirm each input document exists and
validates -- `legion document view <id>` and
`legion document validate --schema <schema-id> --file <payload>` (resolve the schema id by
its `x-doc-type` from `legion document list --doc-type schema --json`). The store refuses
invalid writes anyway; the gate keeps a step from starting on a half-landed input.

## Park and resume

A step that cannot finish -- a round still to run, a question out to real people through
eavesdrop, an operator choice it needs -- parks rather than guesses:

1. The step lands its output as far as it got, status `draft`, with the open items named
   inside it (discover keeps its working file instead, naming the next round).
2. Store a checkpoint reflection naming the exact resume point:
   `legion reflect --repo <repo> --domain checkpoint --text "[SD ANCHOR] resume <step> at <items>; waiting on <what>; inputs: <document ids and working file>"`.
3. Arm a wake, one or both of:
   - timed: `legion defer --work-item sd-<repo>-<step> --repo <repo> --until 1d --note "<what you are waiting for>"`
   - event: `legion signal` to the agent that owes you the input -- eavesdrop for a crawl
     or a question to real people -- so their reply wakes you. Answers from people arrive
     over days; re-arm the timed wake to keep waiting on them.
   Both end as a wake-worthy routing signal from the watch daemon; whichever fires first
   resumes the work.

On wake: recall the anchor (`legion recall --repo <repo> --domain checkpoint --limit 1`),
re-check the thing you were waiting on, and re-dispatch the step with the same inputs. The
draft document or the working file is the state; nothing lives only in a dead session's
context.

## When the intent revises after artifacts land

The root input is a living document; a revision after downstream artifacts exist
propagates by what actually changed:

- **Discovery**: a changed or new intent statement raises new questions. Re-dispatch
  `legion:sd-discover` with the Discovery's working file; the questions already answered
  keep their status until new evidence moves them.
- **Personas and journeys**: move only when a question their citations answer moved, or a
  group gained or lost stories.
- **Blueprints**: relabel planned to real only for what the revision marks settled AND
  actually shipping. Newly proposed items stay planned or absent.
- **Ecosystem**: revises last, from the documents that moved. Open register entries the
  revision answers are recorded as answered, and each answered entry's prediction is
  witnessed here, by you (Instrumentation below).

## What this skill holds to

- Steps in order, each started only on input documents that validate.
- An intent before any artifact.
- Questions the world can answer go to the world; the operator gets strategy and values
  choices, each with a recommendation.
- The first diamond alone: `sd-write-spec` and the second and third diamonds run from their
  own entry points.

## Instrumentation

The judgment predictions live in the agents, each under its own Predictions section: a
question's status holding, a register entry's drafted answer holding, a document surviving
the crit, each with its named witness. The conductor's own emission is the step-completion
claim -- that the step lands its output through the gate on this dispatch, without
parking -- and its confidence is read from the step's inputs, as a judgment: sd-discover on
a fresh intent sits near 0.3, since it parks by design; a writer whose inputs validate and
whose group stands on many authors sits near 0.9; an ecosystem pass with questions likely
out to real people sits near 0.6. Before each step, one line:

```
legion uncertainty emit --surface legion.sd --feature-key sd.<step> \
  --session-id "$CLAUDE_CODE_SESSION_ID" --orphan-ttl-days 180 --input-fingerprint <repo>-<step> \
  --claimed-confidence <p> --payload '{"repo":"<repo>","step":"<step>"}'
```

It resolves by the gate, a store-readable fact: the step's document landed, parked as a
draft, or was refused. The conductor leaves it unwitnessed, because the confidence staked is
a judgment even though the outcome is not; it is the natural first case for store-resolved
witnessing (#1091). Emit it for every step.

### What the conductor carries and witnesses

- **Carry prediction ids forward.** Each agent's return lists its prediction ids. The
  ecosystem's register, with each entry's prediction id and where it went, stays with you
  until every entry is answered: no document carries it. A park anchor carries them when a
  session stops.
- **Witness register entries** when their answers land -- from real people when you wake
  the ecosystem step on them, from the operator at the revision that records the choice --
  under sd-ecosystem-imagine's rule ("Who witnesses, and when").
- **Witness question statuses when a question reopens.** A re-dispatched discovery run over
  newer discourse is the witness for the earlier run's question predictions, under
  sd-discover's rule; the run that emitted a prediction leaves it to that later run.
- **The intent review's claim predictions** have no witness in this pipeline: the Discovery
  answers questions, and claims no longer receive verdicts. Carry their ids in the park
  anchor; they orphan at 180 days unless the operator witnesses them by direct judgment.

### Emit mechanics

These bind the conductor's emits; each agent carries the same rules for its own.

- Pass `--session-id` from the `CLAUDE_CODE_SESSION_ID` environment variable and leave out
  `--model`: the engine resolves the model from that session's live statusline sample,
  and a guessed `--model` mislabels the row into a real cohort. If the variable is unset,
  emit anyway and say so in the report; the rows will sit in the `unknown` cohort.
- Emit exits 0 even when it recorded something you did not mean, and the engine has no
  read-back command: `witness` takes only the id emit printed. Check the command line you
  ran against the ids you meant, before you report.
- Every emit in this pipeline sets `--orphan-ttl-days 180`, since crits, answers from
  people, and re-listens rarely happen inside the 30-day default.
- Record each prediction id in the report next to the thing it is about, with whatever the
  fingerprint was built from, so the witness can rebuild the string to confirm the id. The
  report, the park anchor, and the fingerprint are how the witness finds it; landed
  documents stay as they landed.
- Emission is non-blocking: a failed emit logs and exits 0, and the run continues. A step
  that lands its output and emits nothing has skipped a step; say so in the report.
- The emitter stakes and the named witness scores, always two different parties. A
  mechanical-looking outcome earns no carve-out while the confidence staked against it is
  still a judgment.
