# Rannoch edges and basics toy: how to rerun it

The predictions are in [PREDICTIONS.md](PREDICTIONS.md) and the outcomes are in [RESULTS.md](RESULTS.md). The toy's code is on rannoch's branch `toys/edges`, under `toys/edges`. It sits outside rannoch's crates and builds against them by path, at rannoch main 24ef0b7. The toy is its own Cargo workspace, so rannoch's root `Cargo.toml` stays as it is.

## Run it

```sh
git -C <rannoch checkout> switch toys/edges
cd <rannoch checkout>/toys/edges
./run.sh                    # every step; outputs land in out/
./run.sh edge2 edge3        # only those steps
LINES=20000 ./run.sh basics # basics with 20,000 extended order lines (the default)
KILLS=100 ./run.sh edge5    # edge 5 with 100 kills (the default)
```

`run.sh` does four things:

1. Regenerates Westwind's `seed.sql` with `westwind/generate.py`, the team's seeded generator copied from rannoch `toys/workers`. The toy refuses a seed whose SHA-256 is not `6d38971d...`.
2. Builds the toy in release.
3. Builds `old/`, a separate crate that depends on rannoch at e81e263 by git. This is the last first-parent commit on main before #257 (#256) and #258 (#253).
4. Runs each step and writes its output to `out/<step>.txt`.

## Steps

| step | what it runs | prediction |
|---|---|---|
| `plans` | `per-batch` on 12,000 lines, then `EXPLAIN QUERY PLAN` of the version reads fill runs, with the bundled SQLite | diagnosis for basics 1 |
| `basics` | `rannoch-probe` on `out/`, then Westwind through `Queue::accept` and the heartbeat commit: three runs holding at most 4,096 past the watermark, one at 16,384 and one with no cap; dbstat on the first file; then plain SQLite three times on the same facts, in 4,096-fact transactions, with an index on every foreign key | basics 1 and 2 |
| `per-batch` | direct `Graph::commit` of 4,096 facts at a time, each one timed | diagnosis for basics 1 |
| `edge1` | 36,668 Westwind facts (orders from 2015), with 10% held back 1,000 to 10,000 facts and committed later through the queue in flushed batches of 500; then A-B-A histories; then every current, as-known and as-of read against an in-memory oracle | edge 1 |
| `edge2` | exact copy, different body, the different body again, and the re-id'd body, each through `Graph::commit` and through the queue | edge 2 |
| `edge3` | a version known now, one an hour ahead, one known now again; reads and a cursor reader | edge 3 |
| `edge4` | an undeclared class through the queue and through `Graph::commit`; a `RAISE(ABORT)` trigger on `_versions` for keys `fault-%` | edge 4 |
| `edge5` | 100 runs of a child that accepts through the queue (at most 256 held) on one file, each SIGKILLed between its first commit and three times that; checks after each kill | edge 5 |
| `edge6` | two processes, each with its own handle and queue on one file, sending 5,000 shared bodies and 20,000 of their own | edge 6 |
| `edge7` | the e81e263 file, copied and opened by main: reads, a redelivered old event, a new commit, and the schema before and after | edge 7 |

## Settings

The store sets WAL, `synchronous=FULL` and `fullfsync=ON` itself. The SQLite comparison sets the same three. Facts are Westwind rows as `rannoch.node.<class>` `CloudEvents`, with their foreign keys as text properties and no links, the shape of the earlier 77,688 rows/s and 268 B figures. Ids are `UUIDv7`, minted from each fact's event time and content.

## The 46e7150 comparison

The diagnosis in RESULTS also runs the same toy against rannoch 46e7150, after #253 and #256 and before #261 (#259). To repeat it, do the following. Nothing is committed.

```sh
git -C <rannoch checkout> worktree add --detach /tmp/rannoch-46e7150 46e7150
mkdir -p /tmp/rannoch-46e7150/toys/edges
cp -R toys/edges/Cargo.toml toys/edges/src toys/edges/westwind /tmp/rannoch-46e7150/toys/edges/
cd /tmp/rannoch-46e7150/toys/edges && (cd westwind && python3 generate.py) && cargo build --release
./target/release/edges-toy per-batch westwind out 12000
./target/release/edges-toy basics westwind out 1000000
```
