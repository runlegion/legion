# Salience toy: results

Run 2026-10-30 on 27,147 live reflections (`toy.py`, raw output in `RESULTS.txt`). Predictions in `PREDICTIONS.md`, written before the run.

| Prediction | A age | B use | C keepers | Held? |
|---|---|---|---|---|
| P1 rafters March recalled>=2 rank (median) | 92.5% | 78.0% | 78.0% | No: B leaves them in the bottom half, 0 of 17 in the top 10% |
| P2 rafters recalled>=2 above floor | 40.9% | 52.7% | 52.7% | No (predicted >= 90% under B) |
| P3 never-recalled, >60 days, below floor | 70.9% | 70.9% | 70.2% | No (predicted >= 80%) |
| P4 identity/workflow below floor | 18.7% | 17.3% | 0.0% | Yes |
| P5 all memories above floor | 53.3% | 53.9% | 54.4% | No (predicted < 25%) |

## What it shows

The rules are not what failed. The use signal is. Legion barely records use:

- Only 1,301 of 27,147 memories have ever been recalled, and the most-recalled has a count of 10.
- About 120 to 280 memories a month get a `last_recalled_at`, across the whole team.
- The 17 rafters March memories recalled at least twice were last recalled between March and August 2026. By the record none is in use now, although Sean says rafters' first-week touchpoints come up all the time.

So use-based decay built on today's signal forgets exactly what it should keep, and all three rules come out nearly the same because there is almost no use to tell them apart.

One part held: keeper classes. Declaring identity and workflow as no-decay kept all 780 above the floor, where both decay rules dropped 135 to 146 of them.

## What the design needs before the rules can be judged

1. Every time a memory comes up should be recorded with its time and context: a recall hit, a session-start injection, a consult result, a relay read. Today most of that leaves no trace, or only moves a counter.
2. "Helped" and "judged wrong" should be facts with a scope, so boosting and lessening are recorded per kind of action (the scoped-judgment toy's finding).
3. Then rerun this toy with a fair split: rules computed from use up to a date, judged by what came up after it.
