# Next toy: predictions

Written 2026-11-08, before anything is built; revised the same day before anything was built (operator: "Why are benchmarks running when we're toying a few different ways to build next?"). It compares ways to give a lane's worker only the committed events of its own types, so it never takes, and never writes for, an event it does not handle (rannoch issue #241). Nothing is timed.

## The ways compared

On rannoch main (after #238), outside the rannoch crates, each a toy copy of what it changes:
1. **Store, indexed column.** `_events` gains the event's type in its own column with an index on (type, seq); `next(worker, types)` returns the next committed event after the worker's place whose type is in `types`.
2. **Store, from the body.** `next(worker, types)` filters on the type read from the event's JSON body (`json_extract`), with no new column.
3. **In the lane.** The lane reads `_events` by cursor itself, checks each event's type in Rust, and records its place only for events it takes. The store is unchanged.

## The data

The fill bench stream (fillbench.rs from origin/chore/205-release-check, 306,000 events), with 1% of its events given the type `slow` in place of their own.

## Predictions, for each way

1. A worker for `slow` is handed exactly the committed events of that type, in order, and nothing else; after a restart it resumes from its place.
2. It writes nothing for the 99% it does not handle: its place moves only on the events it takes.
3. Whether it scans the events it skips, read from SQLite's query plan (`EXPLAIN QUERY PLAN`) for ways 1 and 2, and from rows read for way 3: way 1 seeks; ways 2 and 3 scan.

## What breaks a way

- It hands out an event of another type, or misses one of its own.
- It writes for an event it skips.
