# The design discovery workshop

The first diamond: from an intent to a defined problem and the service as it is today, done by agents.
Read `references/double-diamond.json` first: what service design is, where each step sits, and each
agent's input and output. Do this well, and then there will be code.

| Step | Lives in |
|---|---|
| 1. Intent | `skills/intent/` (the conversation, with Rewind) and `agents/not-hotdog.md` (the isolated judge) |
| 2. Stories | `agents/sd-stories.md` |
| 3. Personas, journeys, blueprints | `agents/sd-write-persona.md`, `sd-write-journey.md`, `sd-write-blueprint.md` |
| 4. Ecosystem, written last | `agents/sd-ecosystem-imagine.md` |
| Conductor | `skills/workshop/SKILL.md` |

`references/` holds the documents the agents read by slug (`double-diamond`, `sd-primer`); `schemas/`
holds the question-based Discovery schema; `toys/not-hotdog/` holds the measurements behind not-hotdog.

These are definitions, not code. They become the SD crate's data after the 0.5 core crates land, so the
same workshop runs inside legion and standalone in rafters+.
