# Next toy: predictions

Written 2026-11-08, before anything is built. It tests one change the operator raised for rannoch issue #241: the store's `next` hands out the next committed event of a worker's types, so a lane's worker never takes, and never writes for, an event it does not handle.

## The change tested

On rannoch main (after #238), outside the rannoch crates, in a toy copy of the store:
- `_events` gains the event's type in its own column, with an index on (type, seq).
- `next(worker, types)` returns the next committed event after the worker's place whose type is in `types`, by that index.

## The data

The fill bench stream (fillbench.rs from origin/chore/205-release-check, 306,000 events), with 1% of its events given a second type, the "slow" type.

## Predictions

1. A worker given the slow type is handed exactly the committed events of that type, in order, and nothing else; after a restart it resumes from its place.
2. Taking the slow worker's events writes nothing for the 99% it does not handle: its place moves only on the events it takes.
3. The time for `next` on the slow worker does not grow with the number of skipped events: the same at 1% as at 0.01%.
4. The type column and its index cost the fill bench no more than 5% (median of 3 runs each, with and without, nothing else building).

## What breaks the idea

- `next` with types scans the skipped events.
- The index costs the fill bench more than 15%.
