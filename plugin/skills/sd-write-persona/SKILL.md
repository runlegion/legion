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
   its `required` and `properties` as the contract (today: `meta`, `identity`, `goals`,
   `behaviors`, `frustrations`, `would_leave_if` required; `relationship_stages`,
   `doesnt_care_about`, `quotes`, `moment_of_truth` in the shape). The schema on the day
   you run wins over this sentence.
3. Fill with traced content only; the trace is structural, not prose. The structure is
   the persona tradition's (the primer, NN/g): designers do not speak in absolutes, so
   nothing here carries a priority grade -- importance lives in the narrative, and the
   spec boundary derives any SHALLs later, from evidence, with the derivation shown.
   - `identity` requires `description`, `mental_model`, and `quote` -- the quote is the
     actor's voice from the evidence, not an invented line. When the evidence holds no
     verbatim speech (a thesis-and-documents evidence set often has none), either compose
     the line strictly from traced clauses and end it with `(composed)`, or omit the voice
     and flag the gap -- never present a composition as literal speech. `quotes` (plural)
     carries more of the voice under the same rule.
   - `goals` are what this actor is trying to accomplish, in their terms -- plain prose,
     from the thesis's stakes and the discourse, never a feature list.
   - `behaviors` are `{text}` items showing what the discourse and thesis show this actor
     doing.
   - `frustrations` are `{text, pain_theme}` items where `pain_theme` is the painmatrix
     theme id (T1, T2, ...) -- the trace is a field the schema carries. A killed theme is
     not a frustration; it stays out. A blocked theme is not usable either (check the
     theme's `status` field).
   - `would_leave_if` carries the failure modes that matter to this person -- the
     must-haves expressed as consequences, which is how the tradition carries importance
     without absolutes. Each entry should trace to a proven pain or a thesis stake.
   - `doesnt_care_about` is the explicit negative space: concerns this actor does not
     hold, so nobody designs for them on this actor's behalf.
   - `relationship_stages` is the discovery-to-advocacy table: per stage, what the actor
     does and what the service provides. Planned-future stages carry the standard mark.
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
  (Demographics enter only when they affect behavior: using Cursor daily matters; the
  city does not.)
- Any priority grade or normative keyword on a need, goal, or anything else -- absolutes
  are spec speech, and they derive downstream from evidence, never here.
- A frustration with no painmatrix theme behind it, or one built on a killed pain.
- Describing the actor using a feature the thesis's `current_state.real` does not carry
  (no vaporware in the persona's world either).
- Writing more than one persona in a single pass -- compression suffers when batched.
