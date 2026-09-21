---
name: sd-write-blueprint
description: |
  Write one service blueprint as a schema-valid legion document: the frontstage and
  backstage of a service across a sequence of steps, grounded in the journey it supports
  and the intent's real capabilities. Planned machinery is named as planned, never drawn as
  running. Invoke after the journey it backs exists.
version: 0.1.1
user-invocable: true
allowed-tools: Bash, Read
---

# Blueprint writer: what actually happens behind the journey

A blueprint answers, for each step of a journey: what the actor sees (frontstage), what
the system and people do out of sight (backstage), and where the seams between them fail.
Its honesty rule is tense: things that exist are drawn as existing, things that are
planned are labeled planned, and nothing else appears at all.

## Procedure

1. Read the inputs by id: the journey this blueprint backs, the Ecosystem, the intent.
   The blueprint's steps follow the journey's phases; backstage content comes from the
   intent's `current_state.real` (what exists) and `direction` (what is planned).
2. Draft against the live schema: resolve by `"x-doc-type": "blueprint"` from
   `legion document list --doc-type schema --json`; its `required` and `properties` are
   the contract. Today: `meta` requires `title`, `persona`, `trigger`, `scope`,
   `channels`, `status`, `date`, and `author` (status enum draft/review/done; `author` is
   the invoking agent's identity, the same value as `--owner`); each step requires
   `number`, `title`, `emotional_score` (valence -3 to 3), `emotional_label` (a plain
   word), and `layers` with all five rows -- `evidence`, `customer_actions`, `frontstage`,
   `backstage`, `support`; `frictions` and `metrics` ride as string arrays (`pain_points` is the deprecated old
   field; `fail_points` -- Shostack's F marks -- are process risk, distinct from both), and
   `evidence_links` carries structured citations. An `evidence_links` entry for a
   store-internal source uses `legion://document/<id>` as its url; a repo issue uses its
   issue URL. Two optional step fields come from the discipline's origin: `time` (the
   step's execution time or duration -- Shostack's 1984 blueprints carried per-step times
   and tolerances; hers was a control artifact, and time is what made it one) and
   `fail_points` (her F marks: where the SERVICE can break at this step). Fail points are
   not frictions -- a fail point is process risk, a friction is customer hurt; the
   false-positive that destroys trust is a fail point even on a step the customer enjoys.
3. Fill with traced content:
   - Frontstage per step mirrors the journey phase -- same touchpoints, same channel --
     and the step's `emotional_score` and `emotional_label` agree with that phase's
     curve; the blueprint does not re-feel the journey. A phase carries a range
     (`emotional_start` to `emotional_end`) and a step carries one score: the step takes
     the phase's `emotional_end` -- the felt state where the phase lands.
   - Backstage and `support` name the real mechanism from `current_state.real`, by its
     real name. The line between the two layers is the customer's awareness (the primer's
     Line of Internal Interaction): `backstage` is work the customer knows happens but
     cannot see inside -- the query their action triggered being processed; `support` is
     infrastructure the customer has no awareness of at all -- the Worker, database, or
     index that processing runs on. A mechanism from `direction` is included only labeled
     as planned -- the convention: `(planned)` appended to the step title, and the planned
     layer text opens with `PLANNED:`. A mechanism in neither does not exist and does not
     appear. The `evidence` layer is what the actor leaves behind (the typed query, the
     saved file), not a citation.
   - Frictions per step cite Discovery insights; the blueprint is where a supported insight
     meets the seam that causes it.
   - Failure modes live at the seams -- where frontstage expectation and backstage
     capability part company. The ecosystem's failure modes and the register feed this.
     The schema has no failure_modes field: they land as `frictions` entries and in the
     backstage/support prose at the seam they describe.
4. Validate, then create:

```
legion document validate --schema <schema-id> --file blueprint.json
legion document create --doc-type blueprint --owner <agent> --surface <surface> --from blueprint.json
```

   `--surface` is the service surface -- the same surface the intent carries.

5. **Emit the prediction** (see Instrumentation below), after the create returns the
   blueprint's id, so the fingerprint names a real document.

6. Report the document id and the prediction id with its claimed confidence. One
   blueprint per invocation.

## Instrumentation

The writer's one judgment is whether the backstage stands as drawn. Tense follows from
`current_state.real` and `direction` by rule and is not a prediction; the blueprint as a
whole is. One prediction per document, under this skill's name, that the crit accepts it
without striking a mechanism as vaporware, a step as phaseless, or a friction as
untraced. Stake it from the traces you laid. Anchors: every step behind a journey phase,
every backstage mechanism named from `current_state.real`, every friction on a supported
insight, and no `PLANNED:` layer, sits near 0.8; some planned steps grounded in settled
proposals, or frictions on bounded insights, sits near 0.6; a blueprint that is mostly
planned machinery from `proposed` direction, with fail points inferred rather than cited
and no measurable metric, starts near 0.4. From the anchor, weigh the weakest step rather
than the count: one backstage mechanism whose real name you had to infer is where the
crit strikes first. Put the step and trace counts in the payload, and the journey id,
since the blueprint's meta carries only the persona.

```
legion uncertainty emit --surface legion.sd --feature-key sd.write-blueprint \
  --session-id "$CLAUDE_CODE_SESSION_ID" --orphan-ttl-days 180 \
  --input-fingerprint <journey-id>:blueprint:<blueprint-id> --claimed-confidence <p> \
  --payload '{"journey":"<journey-id>","steps":<n>,"planned":<n>,"frictions_supported":<n>}'
```

The emit mechanics -- session id and model, the exit-0 rule, the 180-day orphan window,
non-blocking emission, never self-witnessing -- are held once in the sd-service-design
skill (Instrumentation, "Emit mechanics") and bind here. What is this writer's alone:
the check is the fingerprint against the id the create printed, and the report carries
the prediction id.

**Who witnesses, and when.** The **crit** (the acceptance step that moves the blueprint
past `draft`) witnesses it, confirming the id by rebuilding
`<journey-id>:blueprint:<blueprint-id>`. The blueprint's meta has no journey field, so
the pair lives in the report and the payload alone; the crit reads it there.
`outcome_correctness` is the fraction of steps accepted as written, backstage and
frictions included; the label is `shipped` when nothing was struck or relabeled,
`scoped-down` when the crit cut steps or moved a mechanism to planned, `escalated` when
it sent the blueprint back. Until the crit exists as a skill, the operator who moves the
document past `draft` witnesses it by hand with the same rule.

## Refuses

- Backstage machinery that `current_state.real` does not carry, unless labeled planned
  and grounded in `direction`.
- A step with no journey phase behind it, or a friction with no Discovery insight.
- Metrics invented for completeness -- a step with nothing measurable says so.
- Witnessing its own prediction. The writer stakes it; the crit scores it.
