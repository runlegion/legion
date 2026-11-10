# Rannoch edges and basics: predictions

Written 2026-11-09, before anything is built. Runs on rannoch main after #256 (nothing keeps a place) and #253 (an event is keyed by its CloudEvent id), outside the rannoch crates. Operator: "scale the performance down to the basics and we need to look at how the changes we just made work with some edges."

Settings as the real store: WAL, synchronous=FULL, fullfsync=ON. Data: Westwind from its seeded generator, as typed CloudEvents with UUIDv7 ids.

## Basics

1. Facts a second, from `accept` through the heartbeat commit (fill inside it), as a share of this disk's durable rate measured by `rannoch-probe`, and against plain SQLite on the same rows and disk (last measured 77,688 rows a second). Prediction: at least 5x main before #253 (5,000 to 7,500 a second), and at most SQLite's rate.
2. Bytes on disk per fact, against SQLite's 268. Prediction: under 1,000.

## Edges

1. Out of order: 10% of events arrive with ids older than ones already committed. Prediction: every current, as-of and as-known read equals an oracle built from the same events in id order.
2. Same id, different body: a second CloudEvent with an id already in the log but other content. Prediction: the key absorbs it, nothing records it, and the first body is what the store keeps.
3. An id from the future: a UUIDv7 an hour ahead of the clock. Prediction: accepted, logged at its own time, and read as known from then.
4. Fill cannot apply: (a) an event of an undeclared class, and (b) a storage fault forced during fill. Prediction: both leave the event in the log unfilled and the commit returns Ok; (b) is the one that is wrong, since a fault is not an event fill cannot apply.
5. Kill mid-heartbeat, at random, 100 times. Prediction: nothing committed is lost, and no committed event is left unfilled.
6. Two handles on one file, each writing. Prediction: every id is kept once and the log reads in id order.
7. A store file from before #253 and #256 (with `seq`, `_places`, `_work`, `_fill`). Prediction: opening it fails or reads wrongly; existing files are left to a migratr migration (STORE-12).

## What breaks an edge

A read that differs from the oracle, a committed event lost or left unfilled, or a duplicate id that keeps the second body.
