# Judgements: ezmode control

Kept outside the control folder, and control runs read only what their prompt names, so no step sees how earlier runs were scored.

## Every step (2026-10-07)

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

## Confirmation reruns (2026-10-07)

- Blueprint C, after the moments-of-truth and on-stage fix: 5/5 with 2 matches; moments of truth moved from partly to matches.
- Ecosystem C, after the no-machinery fix: 5/5 partly; the judge found no invented machinery (A had claim codes, intake settings, a pay formula) and four of the intent's product connections drawn. Contaminated: it read this table while it sat in the control README; the read-only-what-the-prompt-names rule now closes that.
