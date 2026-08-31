---
name: sd-write-journey
description: |
  Write one service-design journey as a schema-valid legion document: a persona through a
  scenario over time, with an emotional curve carried in plain emotion words drawn from
  evidence. Every low point traces to a painmatrix theme; killed pains do not reappear as
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

1. Read the inputs by id: the persona document this journey walks, the Painmatrix, the
   Ecosystem, the thesis. The scenario must be one the ecosystem's channels and exchanges
   can actually carry.
2. Draft against the live schema: resolve by `"x-doc-type": "journey"` from
   `legion document list --doc-type schema --json`; its `required` and `properties` are
   the contract. Today: `meta` requires `title`, `persona`, `scenario`, `goal`, `status`,
   `date`, and `author` (status enum draft/review/done; `author` is the invoking agent's
   identity, the same value as `--owner`; `meta.persona` carries the persona DOCUMENT's
   id -- the UUID, not a slug or bare title); each phase requires `number`, `title`,
   `emotional_start`, `emotional_end`, and `rows` with `actions`, `thoughts`, `emotions`,
   `touchpoints` (plus optional `pain_points` and `opportunities`).
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
   - Every low point in the curve traces to a painmatrix theme. A killed pain (on-topic
     evidence under the painmatrix's `meta.threshold`) must not reappear as a dip -- that
     is the whole point of having killed it. A blocked or UNSCORED theme cannot anchor a
     dip either.
   - High points trace to value exchanges the ecosystem grounds; no delight the service
     cannot deliver today unless the phase is explicitly marked as the planned future and
     the thesis's direction supports it. The mark is a convention, since the schema has no
     field for it: append `(planned future)` to the phase title and say so in
     `meta.scenario`. When the whole service is unbuilt (a greenfield thesis), every phase
     that touches it carries the mark.
4. Validate, then create:

```
legion document validate --schema <schema-id> --file journey.json
legion document create --doc-type journey --owner <agent> --surface <surface> --from journey.json
```

   `--surface` is the service surface -- the same surface the thesis carries.

5. Report the document id. One journey per invocation.

## Refuses

- Affect beyond the evidence: no invented feelings, no valence numbers more precise than
  the evidence supports, no word taxonomy.
- A dip with no painmatrix theme, or any reappearance of a killed pain.
- Phases touching channels the ecosystem does not carry.
- A scenario for a persona document that does not exist yet.
