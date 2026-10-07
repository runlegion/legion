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

## Judgements so far (2026-10-07)

| Step | Runs | Judged | Repeatable | Fixed after |
|---|---|---|---|---|
| 1 intent | 2 | 10 of 14 (before the refusal fix) | yes | not-hotdog keeps every refusal |
| 2 stories | 2 | no reference; rules self-checked | groups yes, counts drift ~25% (38 vs 48 stories) | control runs reach nothing outside |
| 3 persona | 1 | 6 of 6 (2 match, 4 partly) | not rerun | |
| 3 journey | 2 | 8/8, 12/12, 10/10 before fixed points | yes (same 5 stages) | counts come from the Discovery |
| 3 blueprint | 2 | A: 5/5 twice (1 and 2 match), B: 5/5 (1 match) | yes | moments of truth and other people on stage |
| 4 ecosystem | 2 | A: 5/5 (1 match), B: 5/5 (0 match) | actors and moments yes; register differs | draw connections the intent names, no machinery |

Every step still misses what the inputs never say: the control's words do not say the crash app and API
already exist, so blueprint and ecosystem both write "nothing exists". The ecosystem's other fixed misses
(pricing, the Nexus ban, crash correlation) are operator knowledge absent from the words and the pack.
