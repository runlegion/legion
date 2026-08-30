---
name: sd-write-blueprint
description: |
  Write one service blueprint as a schema-valid legion document: the frontstage and
  backstage of a service across a sequence of steps, grounded in the journey it supports
  and the thesis's real capabilities. Planned machinery is named as planned, never drawn as
  running. Invoke after the journey it backs exists.
version: 0.1.0
user-invocable: true
allowed-tools: Bash, Read
---

# Blueprint writer: what actually happens behind the journey

A blueprint answers, for each step of a journey: what the actor sees (frontstage), what
the system and people do out of sight (backstage), and where the seams between them fail.
Its honesty rule is tense: things that exist are drawn as existing, things that are
planned are labeled planned, and nothing else appears at all.

## Procedure

1. Read the inputs by id: the journey this blueprint backs, the Ecosystem, the thesis.
   The blueprint's steps follow the journey's phases; backstage content comes from the
   thesis's `current_state.real` (what exists) and `direction` (what is planned).
2. Draft against the live schema: resolve by `"x-doc-type": "blueprint"` from
   `legion document list --doc-type schema --json`; its `required` and `properties` are
   the contract. Today: `meta` requires `title`, `persona`, `trigger`, `scope`,
   `channels`; each step requires `number`, `title`, `emotional_score` (valence -3 to 3),
   `emotional_label` (a plain word), and `layers` with all five rows -- `evidence`,
   `customer_actions`, `frontstage`, `backstage`, `support`; `pain_points` and `metrics`
   ride as string arrays, and `evidence_links` carries structured citations.
3. Fill with traced content:
   - Frontstage per step mirrors the journey phase -- same touchpoints, same channel --
     and the step's `emotional_score` and `emotional_label` agree with that phase's
     curve; the blueprint does not re-feel the journey.
   - Backstage and `support` name the real mechanism from `current_state.real`, by its
     real name. A mechanism from `direction` is included only labeled as planned; a
     mechanism in neither does not exist and does not appear. The `evidence` layer is
     what the actor leaves behind (the typed query, the saved file), not a citation.
   - Pain points per step cite painmatrix themes; the blueprint is where a proven pain
     meets the seam that causes it.
   - Failure modes live at the seams -- where frontstage expectation and backstage
     capability part company. The ecosystem's failure modes and the register feed this.
4. Validate, then create:

```
legion document validate --schema <schema-id> --file blueprint.json
legion document create --doc-type blueprint --owner <agent> --surface <repo> --from blueprint.json
```

5. Report the document id. One blueprint per invocation.

## Refuses

- Backstage machinery that `current_state.real` does not carry, unless labeled planned
  and grounded in `direction`.
- A step with no journey phase behind it, or a pain point with no painmatrix theme.
- Metrics invented for completeness -- a step with nothing measurable says so.
