# Control: ezmode

A curated golden example for testing each step of the design discovery workshop. It does not need to
be true; it needs to be repeatable. Every step's input is a frozen file here, so when an agent's
definition changes, rerunning that step on its frozen input shows what the change did and nothing else.

## The frozen chain (2026-10-06)

| Step | Frozen input | Agent | Reference to judge against |
|---|---|---|---|
| 1 intent | `words.md` (the operator adopted it: "yes, thats the core of it") | not-hotdog | `00-ecosystem.md`, What ezmode Is |
| 2 stories | `intent.json` + `evidence-pack.json` (91 citations, no search) | sd-stories | none: scored on the rules (verbatim, cited, at altitude) |
| 3 persona | `discovery.json` | sd-write-persona | Alex and Riley in blueprints 01 and 07 |
| 3 journey | `discovery.json` + `persona-p1.json` | sd-write-journey | blueprint 01's emotional journey |
| 3 blueprint | the journey above + `discovery.json` + `intent.json` | sd-write-blueprint | `blueprints/01-first-crash-report.md` |
| 4 ecosystem | everything above | sd-ecosystem-imagine | `00-ecosystem.md` |

The reference documents are the operator's hand-made ezmode service design in the private vault
(`vault-2026/projects/ezmode/service-design/`), named here by path only. The frozen inputs come from a
web-only run with Reddit, Nexus and Discord out of reach; that is a fixed property of the control, not a
gap to fill.

## How a rerun works

Load the step's frozen input into the store as a control document (surface `control-ezmode`), dispatch
the step's agent with that id, then dispatch `sd-judge` with the step, the output id and the reference
paths. Compare the judgement with the previous run's. No live search, no new crawl: step 2 runs on the
evidence pack alone.

## Judgements so far

- Step 1 (not-hotdog, before the refusal fix): 10 of 14 points against `00-ecosystem.md`.
- Step 3 persona: 6 of 6 points (2 match, 4 partly) against Alex and Riley.
