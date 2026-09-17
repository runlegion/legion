---
name: sd-ecosystem-imagine
description: |
  The step where service design asks and answers every question about what turns the intent
  into a product -- who chooses it, who carries the risk, what it depends on, how value moves,
  what must be trusted, where it breaks -- and lands one schema-valid Ecosystem document
  with the answers built in. Five perspective passes find the questions; the step answers
  them from its own design reasoning and the Discovery, asks real people through eavesdrop
  what neither can settle, and brings the operator only strategy and values choices, each
  with a recommendation. Invoke after sd-discover lands the Discovery.
version: 0.2.0
user-invocable: true
allowed-tools: Bash, Read
---

# Ecosystem: what makes the intent a product

The intent says what the thing is. This step interrogates it until the answer is a
product: a service someone chooses, relies on, pays for in some currency, and can be let
down by. It is the narrowing half of the first diamond, so its job is to decide. A pass
that comes back with a list of open questions has moved its work onto the operator; the
2026-09 smugglr ecosystem did exactly that, with 41 questions and no account of how any
of it happens.

An ecosystem is whatever makes this product work -- people, agents, other products,
standards, credentials, data stores, communities, money. No template decides which; the
intent does.

## Inputs

The intent document and the landed Discovery, by id. Contradicted insights the operator
has not yet ruled on stop this step: wait for the rulings (sd-service-design, Park and
resume).

## The questions

The questions come from this intent, not from a list. These are the ground any product
has to cover; write the specific questions this intent raises under each:

- **Choosing.** Who chooses it, how do they find it, what do they replace, and why would
  anyone switch?
- **Benefit and risk.** Who benefits, who carries the risk, and are they the same party?
- **Dependence.** What does it depend on, who controls that, and what happens when it
  changes?
- **Value.** How does value -- money, time, attention, data -- enter, accrue, and leak over
  the life of the relationship?
- **Trust.** What must be trusted, by whom, and what earns it?
- **Governance.** What governs it: credentials, boundaries, audit, who may see what.
- **Failure.** Where does it break, who notices, what do they see, and how do they recover?

When the customer is an agent (sd-service-design, "The customer may be an agent"), the
agent is the primary actor, and the human behind it is a beneficiary and risk-holder with
no direct touchpoint. Ask each question twice: once for the agent that uses the product
and once for the human who may never know it exists.

## Five passes find the questions

Run five light passes, one seat each, each told to find and answer questions about this
intent from its own angle. A single open pass has been measured to find roughly a quarter
of what the five find together.

1. **actor-walk** -- walk each actor through their day with the service; where do they
   enter, hand off, get stuck, leave.
2. **boundary-walk** -- walk the service's edges: what crosses in and out, what happens at
   each crossing, who owns each side.
3. **second-order** -- for every fix the intent proposes, where does the problem move; who
   inherits it.
4. **value-lifecycle** -- where value and money enter, accrue, and leak, over the life of
   the relationship, not the session.
5. **evidence-adversarial** -- attack the answers with the Discovery: which claimed
   exchanges have no supported insight underneath, which supported insights have no
   exchange serving them.

No answers or examples go into the pass prompts; the seat is the structure.

## Answer them

Every question goes to whoever can answer it, in this order:

1. **Design reasoning -- most of them.** This step is the designer. Decide from the intent,
   the Discovery, and how services like this one work, and write the answer down with its
   reason. "How does a user find it?" has an answer a designer can give; give it.
2. **The Discovery -- some of them.** Where a supported or bounded insight answers the
   question, the answer cites it. A contradicted insight never grounds an answer.
3. **Real people, through eavesdrop -- what neither can settle.** A question whose answer
   depends on how people actually behave, and which the Discovery does not reach, becomes
   a question eavesdrop asks on the right lens. `legion signal` the eavesdrop agent with
   the question and the lens, land the ecosystem as a `draft` with the answers so far,
   and park (sd-service-design, Park and resume). When answers arrive, fold them in and
   redraw.
4. **The operator -- only strategy and values.** What the product should be, whom it
   serves first, what it refuses to do. Each such choice goes up with a recommended
   answer and its reasoning, so the operator can agree in one line. Expect a handful, not
   dozens; a long list means steps 1 to 3 were skipped.

The answers must say how things happen. "Users sync their data" is not an answer; say who
starts the sync, through what, under whose credentials, what they see while it runs, and
what they see when it fails.

## Union, weight, land

- Union the five passes' questions and answers and dedup. An answer three or more passes
  reached independently is CORE -- convergence is the confidence weighting, free; no
  separate ranking step. An answer one pass reached is the diversity payoff; keep it,
  marked single-lens.
- Build the Ecosystem payload from the answers: actors, channels, and value exchanges
  carry what was decided, and moments of truth and failure modes carry where it wins and
  breaks. The schema requires `meta` (with `title`
  and `core_service`), `actors` tiered as `primary`/`secondary`/`tertiary` (primary
  required; each PRIMARY actor carries `entry_point` -- where they first touch the
  service -- and `need` -- what they need from it, one line; the `persona` field is null
  until a persona document is authored for it -- the conductor revises it in after that
  persona lands, never this step and never the persona writer; secondary and tertiary
  actors carry no persona field), `channels` (`name`, `type`, `purpose`, and `users` --
  who uses the channel), `value_exchanges` (`from`, `to`, `gives`, `gets`), and
  `moments_of_truth` (`number`, `title`, `actor`, `success`, `failure`, and
  `why_disproportionate` -- the fourth canonical part: why this moment carries more
  weight than other touchpoints), with `failure_modes` (`failure`, `impact`, `recovery`)
  in the shape; resolve the current schema by `"x-doc-type": "ecosystem"`
  from `legion document list --doc-type schema --json`, validate, create:

```
legion document validate --schema <schema-id> --file ecosystem.json
legion document create --doc-type ecosystem --owner <agent> --surface <surface> --from ecosystem.json
```

(`--surface` is the service surface -- the same surface the intent carries. The ecosystem
schema's meta also requires `status`, `date`, and `author` alongside `title` and
`core_service`; status enum draft/review/done, `author` = the invoking agent.)

- **The register:** what is still open when the document lands -- questions out to real
  people through eavesdrop, and choices waiting on the operator, each with its
  recommendation. Nothing else belongs here: a question this step could answer is
  answered, not registered. Write each entry into the document's `failure_modes`, in the
  order you will report them, naming the question and where it went, and report the
  register to whoever invoked you.
- **Emit the predictions** (Instrumentation below), once the create returns an id: one
  per register entry, staking whether its drafted answer holds. Then report to whoever invoked
  you: the ecosystem id, the register, and beside each entry its prediction id and the
  claimed confidence.

## Instrumentation

Convergence is already this step's confidence weighting; the engine turns that heuristic
into a measurement. The union, the dedup, and the actor tiers are derivations and get no
emission. The register does: one prediction per open entry, that the drafted answer
holds -- that the people eavesdrop asks, or the operator, confirm it rather than
overturn it. Answered questions are not predictions here: their reasons are in the
document, and the crit scores the document as a whole.

Claimed confidence starts from the lens count, and the mapping is fixed so the estimator
can see the heuristic tested: one lens 0.3, two lenses 0.5, three or more 0.7. Then move
by evidence, one step at most, to a cap of 0.85: up when a supported or bounded insight
touches the entry's actor or channel without settling the question; down when the
evidence-adversarial pass found no insight under the drafted answer, or when its
only ground is a `needs_pressure_test` proposal. Put the lens count and names in the
payload; the mapping is worthless if the count is not on the row.

```
legion uncertainty emit --surface legion.sd --feature-key sd.ecosystem-imagine.edge \
  --session-id "$CLAUDE_CODE_SESSION_ID" --orphan-ttl-days 180 \
  --input-fingerprint <ecosystem-id>:edge:<n> --claimed-confidence <p> \
  --payload '{"edge":<n>,"lenses":["actor-walk","second-order"],"route":"world|operator"}'
```

`<n>` is the entry's 1-based position in the landed document's `failure_modes`, where the
register lives. The schema gives register entries no id, so the position is the id: emit
after create, from the landed order, and never reorder `failure_modes` afterwards. The
emit mechanics -- session id and model, the exit-0 rule, the 180-day orphan window,
non-blocking emission, never self-witnessing -- are held once in the sd-service-design
skill (Instrumentation, "Emit mechanics") and bind here. What is this step's alone: the
check is each fingerprint against the landed `failure_modes` order, and the report
carries the ids beside the register, with each entry's route, for the conductor to hand
to the blueprint writers.

**Who witnesses, and when.** An open entry is settled when its answer arrives, and each
kind of answer has an owner:

- **Entries sent to real people** are witnessed by the conductor when it wakes this step
  on the answers and the redrawn ecosystem lands: an answer that confirms the drafted
  one is `shipped` at 1.0, an answer that changes it in part is `scoped-down` at 0.5, an
  answer that overturns it is `abandoned` at 0.0. The redraw itself never witnesses; it
  is the same step judging its own stake.
- **Entries sent to the operator** are witnessed by the conductor at the ecosystem
  revision that records the ruling (sd-service-design, "When the intent revises"): the
  recommendation accepted is `shipped` at 1.0, accepted with changes `scoped-down` at
  0.5, rejected `abandoned` at 0.0.

Every witness confirms the id by rebuilding `<ecosystem-id>:edge:<n>` from the report's
register. An entry nobody answers orphans, the right fate for a question nobody took up.

## Refuses

- Landing a document whose register holds questions this step could have answered, or
  sending the operator a question the world can answer.
- Sending the operator a choice without a recommended answer.
- Answering with what happens but not how it happens.
- Running the passes with answers or examples baked into the prompt.
- Promoting a single-lens answer to core, or dropping it for being single-lens.
- Grounding an answer on a contradicted claim, or on evidence not in the Discovery.
- Witnessing its own register predictions, including on a redraw. The passes stake
  them; the conductor scores them.
