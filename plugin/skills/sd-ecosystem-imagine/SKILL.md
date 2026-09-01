---
name: sd-ecosystem-imagine
description: |
  The imagination step of service design: five light perspective passes over the intent and
  supported insights -- actor-walk, boundary-walk, second-order, value-lifecycle,
  evidence-adversarial -- unioned and weighted by cross-lens convergence, landing one
  schema-valid Ecosystem document plus a register of flagged unknowns. The one step that
  earns multi-pass cost. Invoke after sd-discover lands the Discovery.
version: 0.1.0
user-invocable: true
allowed-tools: Bash, Read
---

# Ecosystem-imagine: five lenses, one map

Imagination is where confabulation is cheapest to commit and most expensive to keep, so
this step alone runs multi-pass. The five lenses are not a tunable parameter; they are
what service design is made of -- actors, boundaries, consequences, value over time, and
evidence -- and a single open pass has been measured to find roughly a quarter of what the
five find together.

## Inputs

The intent document and the landed Discovery, by id. Nothing else: the lenses ground in
supported insights and intent facts, and flag everything they cannot ground.

## The five passes

Run each as a LIGHT pass -- one seat, one instruction: "find edges, ground what you can,
flag what you cannot." No edge-answers in the prompt, no honesty scaffolding (the model
polices its own confabulation; the structure that matters is the seat).

1. **actor-walk** -- walk each actor through their day with the service; where do they
   enter, hand off, get stuck, leave.
2. **boundary-walk** -- walk the service's edges: what crosses in and out, what happens at
   each crossing, who owns each side.
3. **second-order** -- for every fix the intent proposes, where does the problem move; who
   inherits it.
4. **value-lifecycle** -- where value and money enter, accrue, and leak, over the life of
   the relationship, not the session.
5. **evidence-adversarial** -- attack the map with the Discovery: which claimed
   exchanges have no supported insight underneath, which supported insights have no
   exchange serving them.

## Union, weight, land

- Union the five edge lists and dedup. An edge found independently by three or more
  lenses is CORE -- convergence is the confidence weighting, free; no separate ranking
  step. An edge found by one lens is the diversity payoff; keep it, marked single-lens.
- Build the Ecosystem payload from the union. The schema requires `meta` (with `title`
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

- **The register:** everything flagged-not-grounded, split in two: unknowns the world can
  answer (candidate crawls or queries -- feed them back toward sd-discover) and questions
  only the operator can answer. Where the register routes automatically (T7) is
  unconverged design -- until that converges, write the register into the ecosystem
  document's `failure_modes` entries, in the order you will report them, and report it
  to whoever invoked you. Dispatch nothing.
- **Emit the predictions** (Instrumentation below), once the create returns an id: one
  per register entry, staking whether it materializes. Then report to whoever invoked
  you: the ecosystem id, the register, and beside each entry its prediction id and the
  claimed confidence.

## Instrumentation

Convergence is already this step's confidence weighting; the engine turns that heuristic
into a measurement. The union, the dedup, and the actor tiers are derivations and get no
emission. The register does: one prediction per flagged edge or unknown, that it
materializes -- that downstream work confirms it as a real edge of the service rather
than dropping it as one lens's invention. Grounded edges are not predictions here: the
Discovery already carries their evidence, and the crit scores the document as a whole.

Claimed confidence starts from the lens count, and the mapping is fixed so the estimator
can see the heuristic tested: one lens 0.3, two lenses 0.5, three or more 0.7. Then move
by evidence, one step at most, to a cap of 0.85: up when a supported or bounded insight
touches the edge's actor or channel without grounding the edge itself; down when the
evidence-adversarial lens flagged the edge as having no insight under it, or when its
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

**Who witnesses, and when.** An edge materializes or does not at a named point
downstream, and each point has an owner:

- The **crit** on the ecosystem, once it exists as a skill, witnesses every register
  entry it rules on: kept as a real edge is `shipped` at 1.0, cut is `abandoned` at 0.0.
- Until then, **sd-write-blueprint** witnesses entries routed to the world, because the
  blueprint is where an edge meets machinery: an entry that lands as a step's fail point,
  friction, or backstage seam materialized (`shipped`, 1.0); an entry the actor's journey
  and blueprint could not carry, reported dropped, did not (`abandoned`, 0.0). An entry
  a later listening pass answers first is witnessed by that pass under sd-discover's
  verdict rule, since by then it is a claim.
- The **conductor** witnesses entries routed to the operator, at the ecosystem revision
  that records them resolved (sd-service-design, "When the intent revises"): answered as
  real is `shipped` at 1.0, answered as not a thing is `abandoned` at 0.0.

Every witness confirms the id by rebuilding `<ecosystem-id>:edge:<n>` from the report's
register. An entry nobody reaches orphans, the right fate for an unknown nobody looked at.

## Refuses

- Running the lens passes with edge-answers or examples baked into the prompt.
- Promoting a single-lens edge to core, or dropping it for being single-lens.
- Grounding an exchange on a contradicted claim, or on evidence not in the Discovery.
- Dispatching the register anywhere.
- Witnessing its own edge predictions. The lenses stake them; the crit, the blueprint
  writer, and the conductor score them.
