# Purge swap toy: predictions

Written 2026-11-07, before anything is built. It tests the one part of the rannoch specs (legion evidence/rannoch-specs at 86c0c65e) held at under 0.4 confidence: whether a purge can rewrite the store without the purged content and swap it in while the fast lane keeps writing, given SQLite's single writer.

## The question

trash.md: a purge removes the content of anything marked dead from the store and the log, in the slow lane (`rannoch-queue-trickle`), and the main path never waits on it. lanes.md, open 2: whether the slow lane's swap of a rewritten store can happen without pausing the fast lane.

## The toy

On rannoch v0.7.0-pre (44f07b4), outside the rannoch crates: a store file taking a steady stream of writes through rannoch's own write path, while a second thread purges a set of dead facts by rewriting the store to a new file without them and swapping it in. Data: rannoch's own fill bench stream (fillbench.rs from origin/chore/205-release-check), with 1% of its facts marked dead and then purged.

## Predictions

1. After the swap, no purged fact's content is in the store file, its WAL or its free pages (searched byte for byte).
2. Every fact not purged reads exactly as before the purge, as of and as known.
3. Writes made during the rewrite are not lost: they are in the store after the swap.
4. The fast lane's longest pause during a purge is under 100 ms, and its rate over the purge is at least 80% of its rate without one.

## What breaks the idea

- A write made during the rewrite is lost or doubled.
- The swap needs the fast lane stopped for longer than 1 s.
- Purged content survives anywhere in the store's files.

Not measured here: the log's own rewrite, which is the same kind of copy on a plain append-only file.
