# Control: ezmode

A fixed product for testing each step of the design discovery workshop against known-good documents.
ezmode's service design was written by hand by the operator; it is the reference. The reference
documents stay in the operator's private vault and are named here by path only.

## Inputs

- **Step 1 words:** `~/.claude/intent/control-ezmode-words.md` (the operator adopted the summary in it,
  2026-10-06: "yes, thats the core of it").
- **Control intent:** composed by not-hotdog from those words, landed as an intent document with
  surface `control-ezmode`.

## Reference (vault-2026/projects/ezmode/service-design/)

| Step | Reference |
|---|---|
| 3 personas | the personas named in the blueprints: Alex (player), Sarah (mod author), Jess (moderator), Alex (developer), the guild leader |
| 3 blueprints | `blueprints/01-first-crash-report.md` ... `09-third-party-integration.md` |
| 4 ecosystem | `00-ecosystem.md` |

The reference is the design at a point in time and holds its own how (stack, schema, prices); the
judge compares at the step's altitude, not the machinery.

## How a run is judged

`sd/agents/sd-judge.md` compares one step's output with its reference on the step's own points and
returns what matches, what the output adds, and what it misses. Rerun a step on this control
whenever its agent's definition changes.
