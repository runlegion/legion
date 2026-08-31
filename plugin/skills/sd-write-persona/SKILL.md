---
name: sd-write-persona
description: |
  Write one service-design persona as a schema-valid legion document: behaviors, mental
  models, and the relationship to the service, every claim tracing to a painmatrix theme or
  a thesis field. Not demographics, not archetypes, no invented interiority. Invoke after
  the painmatrix and ecosystem are landed.
version: 0.1.0
user-invocable: true
allowed-tools: Bash, Read
---

# Persona writer: evidence in, one persona out

A persona here is a compression of evidence, not a character sketch. If the painmatrix and
thesis cannot support a sentence about this person, the sentence does not get written.

## Procedure

1. Read the inputs by id: the thesis, the Painmatrix, the Ecosystem
   (`legion document view <id> --json` each). The persona's actor must exist in the
   ecosystem's actors; do not invent a new actor at this step.
2. Draft the payload against the live schema: resolve it by
   `"x-doc-type": "persona"` from `legion document list --doc-type schema --json` and read
   its `required` and `properties` as the contract (today: `meta`, `identity`,
   `behaviors`, `frustrations`, `needs`, with `moment_of_truth` in the shape). The schema
   on the day you run wins over this sentence.
3. Fill with traced content only; the trace is structural, not prose:
   - `identity` requires `description`, `mental_model`, and `quote` -- the quote is the
     actor's voice from the evidence, not an invented line. When the evidence holds no
     verbatim speech (a thesis-and-documents evidence set often has none), either compose
     the line strictly from traced clauses and end it with `(composed)`, or omit the voice
     and flag the gap -- never present a composition as literal speech.
   - `behaviors` are `{text}` items showing what the discourse and thesis show this actor
     doing.
   - `frustrations` are `{text, pain_theme}` items where `pain_theme` is the painmatrix
     theme id (T1, T2, ...) -- the trace is a field the schema carries. A killed pain
     (on-topic evidence under the painmatrix's `meta.threshold`) is not a frustration; it
     stays out. A blocked or UNSCORED theme is not usable either.
   - `needs` are `{text, priority}` items with priority SHALL, SHOULD, or MAY, derived
     from surviving pains and the thesis's reason-to-exist, phrased as what the actor
     needs.
   - `moment_of_truth` carries `description`, `success`, `failure` -- the one moment this
     persona judges the service by.
   - `meta` requires `set`, `actor`, `status`, `date`, and `author` alongside `title`
     (status enum here is draft/review/done -- NOT the thesis schema's final; `author` is
     the invoking agent's identity, the same value as `--owner`). `actor` must match the
     ecosystem's actor name. The ecosystem's `actors.primary[].persona` field may hold a
     slug placeholder or null; this writer never revises the ecosystem -- naming the landed
     persona there is the conductor's move, after this invocation returns.
4. Validate, then create:

```
legion document validate --schema <schema-id> --file persona.json
legion document create --doc-type persona --owner <agent> --surface <surface> --from persona.json
```

   `--surface` is the service surface -- the same surface the thesis carries (the product
   name, not a git repo).

5. Report the document id to the caller (or the conductor). One persona per invocation;
   run the skill again for the next actor.

## Refuses

- Demographics, names-for-flavor, or any interiority the evidence does not show.
- A frustration with no painmatrix theme behind it, or one built on a killed pain.
- Describing the actor using a feature the thesis's `current_state.real` does not carry
  (no vaporware in the persona's world either).
- Writing more than one persona in a single pass -- compression suffers when batched.
