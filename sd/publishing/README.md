# Publishing the workshop's documents

Current choice (Sean, 2026-10-07): legion's UI shows the workshop's documents, and legion builds the components:
mostly rafters composites, assembled from rafters' own components. Rafters does not build them. Revisable.

The writers land JSON that validates against a schema. A component takes one document's payload as its props
and draws the industry-standard visual from data alone: no prose parsing, no markdown, no hand-drawn charts.
A field a component needs and the schema lacks is a schema change, made here first.

| Document | Schema | Component | Draws | From |
|---|---|---|---|---|
| intent | intent | IntentSheet | who, why, what it refuses, becoming | `what_it_is`, `directions`, `boundaries`, `current_state`, `open_questions` |
| discovery | 01a0592e | QuestionBoard | each question by status (answered, contested, open, few voices) with its voices and citations; persona groups with story counts | `questions[]`, `citations[]`, `stories[]`, `persona_groups[]` |
| persona | 019eb45a-2fe8 | PersonaCard | identity and quote, goals, frustrations, moment of truth, would leave if, relationship stages; thin and open items marked | the schema's fields; `(thin)` and `open_questions` |
| journey | 019eb45a-2ff0 | JourneyMap | stages left to right, an emotion curve from each phase's start and end, lanes for doing, thinking, feeling, touchpoints, pains, opportunities | `phases[].emotional_start/end`, `phases[].rows` |
| blueprint | 019eb45a-2ff9 | ServiceBlueprint | the grid of steps by layer (evidence, customer actions, frontstage, backstage, support) with the lines of interaction and visibility between them; one lane per actor; moments of truth marked; frictions and fail points pinned to steps | `steps[]`, `steps[].layers`, `steps[].actor`, `actors[]`, `moments_of_truth[]`, `frictions`, `fail_points` |
| ecosystem | 019eb45c-fb54 | EcosystemMap | actors in rings by tier, channels, flows drawn by kind (person, product, money, knowledge, trust), moments of truth, the failure register with open and answered entries | `actors`, `channels`, `value_exchanges[].kind`, `moments_of_truth`, `failure_modes` |

Every citation id a component shows links to the Discovery citation (quote, author, source URL), so a reader can
check any statement against the words it came from.

Test data: the ezmode control chain and legion 0.5's chain (ids in `sd/judgements/` and legion memory).
