# ETL kinds toy, second sample: predictions

Written 2026-11-03, before this sample was drawn, and after the first toy's results.

## The question

Does the list from the first toy hold on rows it did not teach, including rare kinds? Two blind readers apply the same closed list to the same 300 fresh rows, and either may say a row does not fit.

The list: `message`, `decision`, `lesson`, `state`, `finding`, `fact`, `identity`, `musing`, `noise`.

The sample leans on purpose, so its percentages are not population estimates: 60 rows from the `identity` domain, 60 from domains that look like kinds (`gotcha`, `incident`, `rfc`, `research`, `feedback`, `doctrine`, `audit`, `reference`, `operating-case-law`), 30 machine checkpoint tails, 30 rows starting `[CHECKPOINT]`, 60 self rows and 60 team rows with no domain. No row from the first sample is used.

## Predictions

1. Each reader says "does not fit" on at most 5% of rows.
2. The readers agree on at least 80% of rows.
3. Of rows where either reader says `fact`, both say it on at least 60%.
4. On `identity`-domain rows, both say `identity` on at least 50%. The domain is loosely used, so not more.
5. Both readers call at least 80% of machine checkpoint tails `noise`.
6. Both readers call at least 80% of `[CHECKPOINT]` rows something other than `noise`.

## What breaks the list

Any of these means the list is not ready to declare:

- More than 15% of rows do not fit for either reader.
- Agreement below 65%.
- `fact` agreement below 40%, which would mean `fact` is not a usable kind as defined.
