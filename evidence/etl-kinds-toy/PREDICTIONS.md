# ETL kinds toy: predictions

Written 2026-11-03, before any sample was drawn or read. The author has seen only aggregate counts of `reflections`: audience, domain frequency, repeated text, and the top repeated strings.

## The question

Can legion's reflections teach us their kinds, or does a kinds list have to be imposed? No kinds list is chosen here. This toy does not load anything into rannoch and does not write to `legion.db`.

## The method

- Draw 500 rows with a fixed seed, across audience, domain state (none, checkpoint, other) and month. No row text is committed.
- Two readers, blind to each other and to these predictions, each read 300 rows: 100 shared and 200 of their own. Each proposes its own kinds and labels its rows, with `noise` for a row nobody would need again, and flags a row that holds more than one kind.
- `toy.py rules` runs mechanical rules over every row, fixed before reading: `checkpoint` from the domain or prefix, a post kind from an `@x verb` marker, and `noise` from text repeated 5 or more times or under 20 characters.

## Predictions

1. Each reader proposes 8 to 15 kinds.
2. On the 100 shared rows, the readers agree on at least 70%, after their kinds are matched by definition.
3. At least 70% of one reader's kinds have a counterpart in the other's.
4. Rules settle 40 to 55% of all rows, and on sampled rows they settle, they match the readers at least 80% of the time.
5. Noise is 5 to 15% of the sample.
6. 10 to 20% of rows are flagged as holding more than one kind.
7. At most half of the domain values in the sample name a kind; the rest name a topic, a project or one effort.
8. At least 90% of sampled team rows are conversation and not durable memory, by the readers' labels.

## What breaks the idea

Either of these means the kinds are not learnable from a reading like this one:

- A reader needs more than 20 kinds.
- Agreement on the shared rows is below 60%.
