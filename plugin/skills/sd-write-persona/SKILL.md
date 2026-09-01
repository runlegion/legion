---
name: sd-write-persona
description: |
  Write one service-design persona as a schema-valid legion document: behaviors, mental
  models, and the relationship to the service, every statement tracing to a Discovery insight or
  an intent field. Not demographics, not archetypes, no invented interiority. Invoke after
  the Discovery and ecosystem are landed.
version: 0.1.0
user-invocable: true
allowed-tools: Bash, Read
---

# Persona writer: evidence in, one persona out

A persona here is a compression of evidence, not a character sketch. If the discovery and
intent cannot support a sentence about this person, the sentence does not get written.

## Procedure

1. Read the inputs by id: the intent, the Discovery, the Ecosystem
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
     verbatim speech (an intent-and-documents evidence set often has none), either compose
     the line strictly from traced clauses and end it with `(composed)`, or omit the voice
     and flag the gap -- never present a composition as literal speech. `quotes` (plural)
     carries more of the voice under the same rule.
   - `goals` are what this actor is trying to accomplish, in their terms -- plain prose,
     from the intent's stakes and the discourse, never a feature list.
   - `behaviors` are `{text}` items showing what the discourse and intent show this actor
     doing.
   - `frustrations` are `{text, insight}` items where `insight` is the Discovery insight id
     -- the trace is a field the schema carries (`pain_theme` is the deprecated old
     field). A contradicted insight is not a frustration; it stays out. A blocked or
     saturated-unevidenced insight is not usable either (check the `status` verdict).
   - `would_leave_if` carries the failure modes that matter to this person -- the
     must-haves expressed as consequences, which is how the tradition carries importance
     without absolutes. Each entry should trace to a supported insight or an intent stake.
   - `doesnt_care_about` is the explicit negative space: concerns this actor does not
     hold, so nobody designs for them on this actor's behalf.
   - `relationship_stages` is the discovery-to-advocacy table: per stage, what the actor
     does and what the service provides. Planned-future stages carry the standard mark.
   - `moment_of_truth` carries `description`, `success`, `failure` -- the one moment this
     persona judges the service by.
   - `meta` requires `set`, `actor`, `status`, `date`, and `author` alongside `title`
     (status enum here is draft/review/done -- NOT the intent schema's final; `author` is
     the invoking agent's identity, the same value as `--owner`). `actor` must match the
     ecosystem's actor name. The ecosystem's `actors.primary[].persona` field may hold a
     slug placeholder or null; this writer never revises the ecosystem -- naming the landed
     persona there is the conductor's move, after this invocation returns.
4. Validate, then create:

```
legion document validate --schema <schema-id> --file persona.json
legion document create --doc-type persona --owner <agent> --surface <surface> --from persona.json
```

   `--surface` is the service surface -- the same surface the intent carries (the product
   name, not a git repo).

5. **Emit the prediction** (see Instrumentation below), after the create returns the
   persona's id, so the fingerprint names a real document.

6. Report the document id and the prediction id with its claimed confidence to the
   caller (or the conductor). One persona per invocation; run the skill again for the
   next actor.

## Instrumentation

The writer's one judgment is whether this compression of evidence stands as written.
Which insights are usable follows from their `status` by rule and is not a prediction;
the persona as a whole is. One prediction per document, under this skill's name, that
the crit accepts it without striking a statement as untraced or adding one the evidence
earned. Stake it from the traces you actually laid. Anchors: every frustration and
`would_leave_if` entry on a supported insight, with a verbatim quote from the evidence,
sits near 0.8; a mix of supported and bounded insights, or a `(composed)` quote, sits
near 0.6; a persona carried mostly by the intent's `actors[].stakes` because the actor's
insights are blocked, or an actor with no verbatim voice anywhere, starts near 0.4.
From the anchor, weigh the weakest statement rather than the count: one `would_leave_if`
entry resting on an intent stake alone is where the crit strikes first, and it lowers
the number more than three bounded frustrations do. Put the trace counts in the payload.

```
legion uncertainty emit --surface legion.sd --feature-key sd.write-persona \
  --session-id "$CLAUDE_CODE_SESSION_ID" --orphan-ttl-days 180 \
  --input-fingerprint <ecosystem-id>:persona:<persona-id> --claimed-confidence <p> \
  --payload '{"supported":<n>,"bounded":<n>,"intent":<n>,"quote":"verbatim|composed|none"}'
```

Pass `--session-id` from `CLAUDE_CODE_SESSION_ID` and omit `--model`: the engine resolves
the model from the session's live sample, a row with neither lands in the `unknown`
cohort where no regression can be seen, and a guessed model mislabels the row into a
real cohort. Emit exits 0 whatever it recorded, so check the fingerprint against the id
the create printed, not the exit code. Do not revise the persona to hold the prediction
id; the report carries it.

**Who witnesses, and when.** The **crit** (the acceptance step that moves the persona
past `draft`) witnesses it, confirming the id by rebuilding
`<ecosystem-id>:persona:<persona-id>` -- the pair is in the report, and in the
ecosystem's `actors.primary[].persona` once the conductor fills it. `outcome_correctness`
is the fraction of the persona's statements (behaviors, goals, frustrations,
`would_leave_if`) accepted as written; the label is `shipped` when nothing was struck or
added, `scoped-down` when the crit cut statements, `escalated` when it sent the persona
back. Until the crit exists as a skill, the operator who moves the document past `draft`
witnesses it by hand with the same rule.

Emission is non-blocking: a failed emit logs and exits 0, and the run continues. A
persona landed with no prediction has skipped a step; say so in the report.

## Refuses

- Demographics, names-for-flavor, or any interiority the evidence does not show.
  (Demographics enter only when they affect behavior: using Cursor daily matters; the
  city does not.)
- Any priority grade or normative keyword on a need, goal, or anything else -- absolutes
  are spec speech, and they derive downstream from evidence, never here.
- A frustration with no Discovery insight behind it, or one built on a contradicted claim.
- Describing the actor using a feature the intent's `current_state.real` does not carry
  (no vaporware in the persona's world either).
- Writing more than one persona in a single pass -- compression suffers when batched.
- Witnessing its own prediction. The writer stakes it; the crit scores it. A
  self-witnessed persona is the rubber stamp the engine exists to catch.
