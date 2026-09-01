---
name: sd-write-journey
description: |
  Write one service-design journey as a schema-valid legion document: a persona through a
  scenario over time, with an emotional curve carried in plain emotion words drawn from
  evidence. Every low point traces to a Discovery insight; contradicted claims do not reappear as
  dips. Invoke after the persona it walks exists.
version: 0.1.0
user-invocable: true
allowed-tools: Bash, Read
---

# Journey writer: one persona, one scenario, over time

A journey is a persona moving through a real scenario, phase by phase, with what they do,
touch, and feel at each phase. The feelings are the part most tempting to invent, so the
rule is structural: emotions are data.

## Procedure

1. Read the inputs by id: the persona document this journey walks, the Discovery, the
   Ecosystem, the intent. The scenario must be one the ecosystem's channels and exchanges
   can actually carry.
2. Draft against the live schema: resolve by `"x-doc-type": "journey"` from
   `legion document list --doc-type schema --json`; its `required` and `properties` are
   the contract. Today: `meta` requires `title`, `persona`, `scenario`, `goal`, `status`,
   `date`, and `author` (status enum draft/review/done; `author` is the invoking agent's
   identity, the same value as `--owner`; `meta.persona` carries the persona DOCUMENT's
   id -- the UUID, not a slug or bare title; optional `meta.expectations` carries what
   the persona expects going in, beside the goal, per the NN/g scenario block); each
   phase requires `number`, `title`, `emotional_start`, `emotional_end`, and `rows` with
   `actions`, `thoughts`, `emotions`, `touchpoints` (plus optional `frictions` (supersedes the deprecated `pain_points`),
   `opportunities`, and `ownership` -- who owns acting on the phase's opportunities).
3. Fill with traced content:
   - Actions and touchpoints come from the ecosystem's channels; a phase cannot touch a
     channel the ecosystem does not have.
   - **The emotional curve is the schema's, not yours to invent:** each phase carries
     `emotional_start` and `emotional_end` as valence numbers from -3 to 3, and the
     phase's `rows.emotions` carries plain emotion words -- frustrated, relieved, wary,
     confident -- taken from what the evidence shows this actor expressing. Place the
     numbers from the evidence's direction and strength; do not manufacture precision the
     evidence lacks (0 is a legitimate value). The word vocabulary is unconstrained -- no
     taxonomy has converged (T6, open design question); never invent one beyond the
     evidence.
   - Every low point in the curve traces to a Discovery insight, named in the phase's
     `frictions` row. A contradicted claim must not reappear as a dip -- that is the
     whole point of the verdict. A blocked or saturated-unevidenced insight cannot
     anchor a dip either (check the `status` verdict).
   - High points trace to value exchanges the ecosystem grounds; no delight the service
     cannot deliver today unless the phase is explicitly marked as the planned future and
     the intent's direction supports it. The mark is a convention, since the schema has no
     field for it: append `(planned future)` to the phase title and say so in
     `meta.scenario`. When the whole service is unbuilt (a greenfield intent), every phase
     that touches it carries the mark.
4. Validate, then create:

```
legion document validate --schema <schema-id> --file journey.json
legion document create --doc-type journey --owner <agent> --surface <surface> --from journey.json
```

   `--surface` is the service surface -- the same surface the intent carries.

5. **Emit the prediction** (see Instrumentation below), after the create returns the
   journey's id, so the fingerprint names a real document.

6. Report the document id and the prediction id with its claimed confidence. One journey
   per invocation.

## Instrumentation

The writer's one judgment is whether this curve stands as drawn. Which insights can
anchor a dip follows from their `status` by rule and is not a prediction; the journey as
a whole is. One prediction per document, under this skill's name, that the crit accepts
it without striking a phase's affect as invented or a dip as untraced. Stake it from the
traces you laid. Anchors: every dip on a supported insight, every high on a value
exchange the ecosystem grounds, valence placed from evidence that shows both direction
and strength, and no `(planned future)` phase, sits near 0.8; dips on bounded insights,
or valence placed from direction alone with 0 where strength is unknown, sits near 0.6;
a greenfield journey where every phase carries the planned mark, or one whose low points
lean on intent stakes because the persona's insights are blocked, starts near 0.4. From
the anchor, weigh the weakest phase rather than the count: one dip whose insight is
bounded exactly where this persona sits is where the crit strikes first. Put the phase
and trace counts in the payload.

```
legion uncertainty emit --surface legion.sd --feature-key sd.write-journey \
  --session-id "$CLAUDE_CODE_SESSION_ID" --orphan-ttl-days 180 \
  --input-fingerprint <persona-id>:journey:<journey-id> --claimed-confidence <p> \
  --payload '{"phases":<n>,"dips_supported":<n>,"dips_bounded":<n>,"planned":<n>}'
```

Pass `--session-id` from `CLAUDE_CODE_SESSION_ID` and omit `--model`: the engine resolves
the model from the session's live sample, a row with neither lands in the `unknown`
cohort where no regression can be seen, and a guessed model mislabels the row into a
real cohort. Emit exits 0 whatever it recorded, so check the fingerprint against the id
the create printed, not the exit code. Do not revise the journey to hold the prediction
id; the report carries it.

**Who witnesses, and when.** The **crit** (the acceptance step that moves the journey
past `draft`) witnesses it, confirming the id by rebuilding
`<persona-id>:journey:<journey-id>` -- the pair is in the report and in the journey's own
`meta.persona`. `outcome_correctness` is the fraction of phases accepted as written,
affect and traces included; the label is `shipped` when nothing was struck or redrawn,
`scoped-down` when the crit cut phases or flattened a dip, `escalated` when it sent the
journey back. Until the crit exists as a skill, the operator who moves the document past
`draft` witnesses it by hand with the same rule.

Emission is non-blocking: a failed emit logs and exits 0, and the run continues. A
journey landed with no prediction has skipped a step; say so in the report.

## Refuses

- Affect beyond the evidence: no invented feelings, no valence numbers more precise than
  the evidence supports, no word taxonomy.
- A dip with no Discovery insight, or any reappearance of a contradicted claim.
- Phases touching channels the ecosystem does not carry.
- A scenario for a persona document that does not exist yet.
- Witnessing its own prediction. The writer stakes it; the crit scores it. A
  self-witnessed journey is the rubber stamp the engine exists to catch.
