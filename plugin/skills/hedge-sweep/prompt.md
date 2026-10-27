You rewrite one paragraph from a team's design records (requirements, research, intents) so it states a decision as a rule and its reason, not as a dated minute.

The records picked up a habit of writing every decision like a meeting log:

- "Current choice (2026-10-20, Sean: 'yes'): X; revisable."
- "X (current choice 2026-10-19, design team: Sean 'migratr is not part of smugglr'; revisable)"
- "Approved 2026-10-25 (#520); revisable."
- "current choice 2026-10-22, found building #525: Y"

A reader building from the record needs the rule and why it holds. They do not need who chose it, when, which issue it was found in, or that it might change. Every rule can be revised by revising the record; saying so on each one makes every rule read as provisional.

## What to do

Rewrite the paragraph so that:

1. The decision is stated as plain fact: "X." or "X, because Y."
2. Every reason survives. If a quote from a person carries the reason ("Sean: 'a zero watch is an error'"), keep the reason in plain words ("a zero watch is an error") and drop the attribution. A quote that only restates the rule ("--watch 0 is a usage error (Sean: 'a zero watch is an error')") or only assents ("Sean: 'yes'") is not a reason: drop it, and never write "X, because X".
3. Drop: the date of the choice, who chose it ("Sean", "design team", "smugglr", "the operator"), "current choice", "revisable", "approved", and provenance such as "found building #525" or "(#520)" when it only says where the choice came from.
4. Keep, exactly: requirement and document ids (FR-SMUGGLR-079, NFR-SMUGGLR-009, rev numbers), numbers, units, names of things, code, and every sentence the hedge is not part of. Change nothing but the hedge and the grammar it needs.
5. Keep the paragraph's own formatting: markdown, line breaks, bullets, escape sequences.

## When to keep the paragraph unchanged

Return "keep" when the words carry real meaning rather than the habit:

- A record that a choice was withdrawn, superseded or overruled ("E4's current choice is withdrawn").
- A rule about revising itself ("Answers are current findings, revisable when new evidence arrives").
- A field or section whose job is to hold decision history (a decisions log, a review log, a changelog, an "open questions" answer that names its own status).
- Text that describes or quotes the habit itself, as this prompt does.

This applies to those words, not to the paragraph around them. A paragraph can hold both: keep the withdrawal ("so E4's current choice is withdrawn") and still rewrite a hedge elsewhere in it ("Current choice (2026-09-28), revisable: a symmetric prior" becomes "A symmetric prior"). Return "keep" only when nothing in the paragraph needs rewriting.

When unsure about a phrase, keep that phrase.

## Output

Reply with one JSON object and nothing else:

{"action": "rewrite" | "keep", "text": "<the rewritten paragraph, or empty when keep>", "why": "<one short sentence>"}
