# ETL kinds toy: results

Run 2026-11-03 on 27,694 rows of `legion.db`, read only. Two blind readers each labelled 300 sampled rows from excerpts of at most 600 characters, 100 of them shared. Neither stop condition was hit: the kinds look learnable. This is one sample, read once by two readers on the same model.

## Against the predictions

| # | Prediction | Result | Verdict |
|---|---|---|---|
| 1 | Each reader proposes 8 to 15 kinds | 8 and 6, each plus noise | Partly: fewer than expected |
| 2 | At least 70% agreement on shared rows | 80 of 100, kinds matched by definition after seeing the labels | Held |
| 3 | At least 70% of kinds have a counterpart | 6 of reader A's 8 | Held |
| 4 | Rules settle 40 to 55%, and match readers at least 80% | 35.9% settled; matched 45% and 43%, or 90% and 84% without checkpoint rows | Missed |
| 5 | Noise is 5 to 15% | 20.0% and 18.3%, mostly automatic checkpoints | Missed |
| 6 | 10 to 20% of rows hold more than one kind | 6.7% and 4.0% | Missed: rows are cleaner than expected |
| 7 | At most half of the domains name a kind | About 80 distinct values in the sample, a small minority kinds; judged by eye, not counted | Held |
| 8 | At least 90% of team rows are conversation | 82% and 68% | Missed |

## What the readers agree on

Independently, both found six kinds, plus noise:

| Reader A | Reader B | Shared rows agreeing |
|---|---|---|
| exchange | message | 28 |
| decision | decision | 10 |
| lesson | rule | 7 |
| state | status | 7 |
| finding | finding | 6 |
| musing | musing | 4 |
| noise | noise | 18 |

Reader A alone has `fact`, a standing description of how something works, and `identity`, 2 rows. Reader B spread `fact`-like rows across rule, finding and decision: that boundary is the main open one. Lesson against finding split 4 rows too.

`identity` shows the sample's limit. Legion's identity rows matter and a 500-row sample barely sees them, so a rare kind can be important and invisible here.

## What was not predicted

**The `checkpoint` domain is mostly not checkpoints.** Of 5,533 rows with the domain or prefix, the readers labelled 80 of the 101 they saw as noise. None of the 80 start with `[CHECKPOINT]`, and 8 of the 21 they kept do. Most are machine-written transcript tails: at least 3,649 start `[SUBAGENT CHECKPOINT]` and 200 start `[COMPACT CHECKPOINT]`. A session's real checkpoint and a hook's transcript tail share one domain. 4,739 of the 5,533 are already archived and 89 have ever been recalled. The prefix separates them where the domain does not; that rule was found after reading, so it has not been tested on fresh rows.

**Team rows are not all chat.** Between 18% and 32% of them hold durable content, by the readers' labels. Audience alone cannot split chat from memory.

## Limits

- Readers saw excerpts, so a long row with a late point could read as noise.
- The sample is stratified, so its percentages are not population estimates.
- Kinds were matched by the author after the labels were in.
- Both readers ran the same model, so agreement can reflect a shared bias and not the data.
