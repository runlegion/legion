# ETL kinds toy, second sample: results

Run 2026-11-03. Two blind readers applied one closed list to the same 300 fresh rows, from excerpts of at most 600 characters. None of the conditions that break the list was hit.

## Against the predictions

| # | Prediction | Result | Verdict |
|---|---|---|---|
| 1 | At most 5% do not fit | 0 for both | Held, though a closed list pushes readers to pick something |
| 2 | Agreement at least 80% | 264 of 300 (88%); 251 of 270 (93%) without machine tails | Held |
| 3 | `fact`: both on at least 60% of rows where either says it | 11 of 16 (69%) | Held, on few rows |
| 4 | `identity` domain: both say `identity` on at least 50% | at most 29 of 60 (48%) | Narrowly missed |
| 5 | Both call at least 80% of machine tails `noise` | reader A 21 of 30, reader B 6 of 30 | Missed badly |
| 6 | Both call at least 80% of `[CHECKPOINT]` rows not `noise` | 30 of 30 for both, 29 of them `state` | Held |

## What it shows

**`noise` is not a kind.** It answers a different question, whether anyone needs a row again, and it is where readers split: `noise` against `state` is 16 of the 36 disagreements, and both said `noise` on only 16 of the 32 rows where either did. On machine tails, one reader judged the transcript useless and the other judged it a record of progress, and each is a fair reading of the same row.

**Machine tails need no reader.** Their prefix identifies them. They are hook output, so what to do with them is one decision about all of them and not a judgement per row.

**The `checkpoint` domain splits cleanly by prefix.** `[CHECKPOINT]` rows are `state`. Their prefix held on fresh rows here, though only 30.

**The `identity` domain is loose.** About half its rows are identity. The rest are decisions and lessons, so the domain cannot be taken as the kind.

**The eight kinds covered everything.** `message`, `decision`, `lesson`, `state`, `finding`, `fact`, `identity` and `musing`. The disagreements left are scattered, a few rows each.

## Limits

- Neither sample has been judged by a person, and all four readers ran one model, so agreement can be a shared bias.
- Excerpts stop at 600 characters.
- The sample leans on the weak spots, so its percentages are not population estimates.
- Rare kinds beyond those sampled on purpose may still be missing.
