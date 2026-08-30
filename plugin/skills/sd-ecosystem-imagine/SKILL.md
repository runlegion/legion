---
name: sd-ecosystem-imagine
description: |
  The imagination step of service design: five light perspective passes over the thesis and
  proven pains -- actor-walk, boundary-walk, second-order, value-lifecycle,
  evidence-adversarial -- unioned and weighted by cross-lens convergence, landing one
  schema-valid Ecosystem document plus a register of flagged unknowns. The one step that
  earns multi-pass cost. Invoke after sd-pain-listen lands the painmatrix.
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

The thesis document and the landed Painmatrix, by id. Nothing else: the lenses ground in
proven pains and thesis facts, and flag everything they cannot ground.

## The five passes

Run each as a LIGHT pass -- one seat, one instruction: "find edges, ground what you can,
flag what you cannot." No edge-answers in the prompt, no honesty scaffolding (the model
polices its own confabulation; the structure that matters is the seat).

1. **actor-walk** -- walk each actor through their day with the service; where do they
   enter, hand off, get stuck, leave.
2. **boundary-walk** -- walk the service's edges: what crosses in and out, what happens at
   each crossing, who owns each side.
3. **second-order** -- for every fix the thesis proposes, where does the problem move; who
   inherits it.
4. **value-lifecycle** -- where value and money enter, accrue, and leak, over the life of
   the relationship, not the session.
5. **evidence-adversarial** -- attack the map with the painmatrix: which claimed
   exchanges have no proven pain underneath, which proven pains have no exchange serving
   them.

## Union, weight, land

- Union the five edge lists and dedup. An edge found independently by three or more
  lenses is CORE -- convergence is the confidence weighting, free; no separate ranking
  step. An edge found by one lens is the diversity payoff; keep it, marked single-lens.
- Build the Ecosystem payload from the union. The schema requires `meta` (with `title`
  and `core_service`), `actors` tiered as `primary`/`secondary`/`tertiary` (primary
  required; each actor's `persona` field is null until a persona document is authored for
  it, and gets revised in later), `channels` (`name`, `type`, `purpose`),
  `value_exchanges` (`from`, `to`, `gives`, `gets`), and `moments_of_truth` (`number`,
  `title`, `actor`, `success`, `failure`), with `failure_modes` (`failure`, `impact`,
  `recovery`) in the shape; resolve the current schema by `"x-doc-type": "ecosystem"`
  from `legion document list --doc-type schema --json`, validate, create:

```
legion document validate --schema <schema-id> --file ecosystem.json
legion document create --doc-type ecosystem --owner <agent> --surface <repo> --from ecosystem.json
```

- **The register:** everything flagged-not-grounded, split in two: unknowns the world can
  answer (candidate crawls or queries -- feed them back toward pain-listen) and decisions
  only the operator can make. Where the register goes automatically (T7) is an open
  operator decision -- until ruled, write the register into the ecosystem document's
  failure-mode notes and report it to whoever invoked you. Dispatch nothing.

## Refuses

- Running the lens passes with edge-answers or examples baked into the prompt.
- Promoting a single-lens edge to core, or dropping it for being single-lens.
- Grounding an exchange on a killed pain, or on evidence not in the painmatrix.
- Dispatching the register anywhere.
