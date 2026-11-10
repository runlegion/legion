# Rannoch edges and basics: results

Run on 2026-11-09 in rannoch `toys/edges` (built against rannoch main 24ef0b7), against [PREDICTIONS.md](PREDICTIONS.md). [README.md](README.md) explains how to rerun it. The machine was an Apple M4 with 16 GB of RAM, and the store sat on `/Volumes/store`, which was 81% full. Other agents were running, so the load average stayed between 2.5 and 5. Each output below is the toy's own, kept in `toys/edges/out/` when it runs.

Settings follow the predictions. The store sets WAL, `synchronous=FULL` and `fullfsync=ON` itself, and the SQLite comparison sets the same three. Data is Westwind from its seeded generator (SHA-256 `6d38971d...` checked). Each row is a `rannoch.node.<class>` `CloudEvent` with a UUIDv7 id minted from its event time and content. Foreign keys are kept as text properties, the nodes-only shape that the earlier 77,688 rows/s and 268 B figures were measured on. A shipped order is a second fact about its order, valid from its shipped date.

**Summary.** Five predictions fail, four hold:

| prediction | outcome |
|---|---|
| Basics 1, rate | fails |
| Basics 2, bytes per fact | fails |
| Edge 1, out of order | fails |
| Edge 2, same id, different body | holds |
| Edge 3, id from the future | holds as worded |
| Edge 4, fill cannot apply | holds through `Graph::commit`; the queue refuses (a) before the log |
| Edge 5, kill mid-heartbeat | holds |
| Edge 6, two handles | fails |
| Edge 7, old store file | fails |

Main's commit cost grows with the size of the store, and that one cause sets the basics numbers and edge 6's lost writes. It came in with #259 (PR #261), the change that came after #253 and #256.

## Basics 1: facts a second

**Prediction.** At least 5 times main before #253 (5,000 to 7,500 a second), and at most SQLite's rate.

**Run.** Westwind went through `Queue::accept` and the heartbeat commit, with fill inside the commit. Timing ran from the first accept until the watermark passed the last ticket, and the bodies were built beforehand.

- The producer held at most 4,096 events past the watermark (three runs), then 16,384 (one run), then had no cap (one run).
- The disk rate came from `rannoch_probe::disk` on the store's directory.
- SQLite loaded the same facts into `westwind/schema.sql` tables, with an index on each of the 8 foreign keys, in 4,096-fact transactions.

The earlier toys loaded 1,000,000 extended order lines (1,754,602 facts). Main cannot load that in reasonable time (see the diagnosis below), so main ran 20,000 lines (36,668 facts).

| run | facts | facts/s | events per commit | load (start / end) |
|---|---|---|---|---|
| main, cap 4,096, #1 | 36,668 | 505 | 9 commits, median 4,096 | 3.44 / 4.76 |
| main, cap 4,096, #2 | 36,668 | 560 | same | 4.76 / 3.37 |
| main, cap 4,096, #3 | 36,668 | 554 | same | 3.37 / 2.76 |
| main, cap 16,384 | 36,668 | 501 | 3 commits, max 16,384 | 2.76 / 5.04 |
| main, no cap | 36,668 | 580 | 2 commits, max 25,649 | 5.04 / 3.18 |
| SQLite, 3 runs | 36,668 | 146,308 to 167,012 | 4,096 | 3.18 |

**Disk.** The probe measured 151.5 MB/s of durable writes and a 6.4 ms durable flush. "Share of the durable rate" here means facts/s × bytes per fact on disk ÷ the probe's bytes/s. On main that is 0.5%: the disk is not what limits it.

**Outcome: fails.** Main reaches 501 to 580 facts a second. That is a tenth of main before #253 (5,000 to 7,500), not 5 times it, and 0.3% of SQLite's rate on the same rows.

**Diagnosis: a commit's cost grows with the number of versions in the store.** Direct `Graph::commit` calls of 4,096 facts each (`out/per-batch.txt`, 30,000 lines) slow steadily as the file grows:

| batch | 0 | 1 | 2 | 3 | 5 | 8 | 12 |
|---|---|---|---|---|---|---|---|
| seconds | 0.83 | 2.40 | 3.94 | 5.49 | 8.62 | 13.73 | 20.82 |
| facts/s | 4,962 | 1,709 | 1,040 | 746 | 475 | 298 | 197 |

Each batch takes about 1.55 s longer than the one before it. At that pace the earlier toys' 1,754,602 facts would take about 40 hours.

The cause is #259 (PR #261). It made `_versions` a WITHOUT ROWID table with `PRIMARY KEY(line, event)`. On the bundled SQLite 3.53.2, `EXPLAIN QUERY PLAN` (`out/plans.txt`) shows how three reads are now planned:

- `current_at`, which runs on every fill: `SEARCH _versions USING PRIMARY KEY (line=?)`. Every version is on line `'main'`, so this reads every version. It took 2.16 ms a call at 22,559 versions.
- `newest_point`, which also runs on every fill: the same plan, 2.12 ms a call.
- `NEWEST_VERSION`, which `find` uses: still `_versions_at`, 0.01 ms.

The same toy, built against 46e7150 (after #253 and #256, before #261) in a scratch worktree, plans both fill reads on `_versions_at` at 0.02 ms a call. Its numbers:

| 46e7150 run | facts | facts/s | B/fact |
|---|---|---|---|
| direct commits, 4,096 each | 22,559 | 53,188 to 59,918, flat | |
| cap 4,096, 3 runs | 36,668 | 38,549 to 39,658 | 1,461 |
| cap 16,384 | 36,668 | 51,255 | 1,461 |
| no cap | 36,668 | 54,253 | 1,461 |
| cap 4,096 | 1,754,602 | **34,121** | 1,464 |
| cap 16,384 | 1,754,602 | **45,385** | 1,464 |
| no cap | 1,754,602 | 40,273 (3 commits, one of 1,142,681 events) | 1,464 |
| SQLite | 1,754,602 | 84,488 | 268 |

So the predicted shape held at 46e7150 and not on main:

- At 46e7150, 4,096-event commits ran 4.5 to 6.8 times main before #253, and 16,384-event commits 6.1 to 9.1 times.
- Both stayed under SQLite's rate on the same disk (84,488).
- Both used 33 to 44% of the disk's durable rate.

On main, the regression hides all of that.

The 20 ms heartbeat also has a cost, visible at 46e7150: the producer capped at 4,096 waits up to one interval for each commit.

A producer with no cap made the batches grow without bound: 3 commits for 1.75M facts, one of them holding 1,142,681 events.

## Basics 2: bytes on disk per fact

**Prediction.** Under 1,000 bytes per fact, against SQLite's 268.

**Run.** File size with its WAL after `PRAGMA wal_checkpoint(TRUNCATE)`, divided by facts, plus dbstat for the breakdown.

**Outcome: fails.** Main stores 1,425 B per fact (52.3 MB for 36,668 facts). SQLite stores 266 to 268 on the same facts, so main is 5.4 times larger. At 46e7150 the figure is 1,461 to 1,464 B at both scales.

Where main's bytes go, per fact:

| table or index | B/fact |
|---|---|
| `_events`, the log body | 588 |
| `_versions`, the history, with a JSON copy of the properties | 471 |
| `_versions_at` index | 130 |
| `class:order_line` | 100 |
| `_events_type` index | 72 |
| `class:order` | 61 |

The log body and the history's property copy alone come to 1,059 B, over the limit before any index. The class tables, which hold the data SQLite also stores, take 161 B.

## Edge 1: out of order

**Prediction.** Every current, as-of and as-known read equals an oracle built from the same events in id order.

**Run.** The data was the seed plus 20,000 extended order lines placed from 2015-01-05, so every id was in the past: 36,668 facts.

- **Delivery.** A seeded draw held back 10% of the facts and released each one 1,000 to 10,000 facts later. Everything went through `Queue::accept`, in batches of 500 with each batch flushed. 3,788 facts (10.3%) arrived with an id older than one already committed.
- **Oracle.** It was built in memory, without rannoch, from the facts in id order. A fact identical to the version before it in id order writes nothing.
- **Reads compared** (175,600 in all):
  - 29,255 current reads (`find`, plus `state_at` for the event id);
  - 73,336 as-known reads (`state_at`, at each version's known-at and 1 ms before it);
  - 73,009 as-of reads (`find_as_of`, on each version's valid day and the day before it).
- **A-B-A.** I added 300 customers whose city goes A, then B, then A, 100 ms apart on one valid day. For 150 of them, the B is committed after both As. The other 150 arrive in order, as a control.

**Outcome: fails.**

- **Westwind:** all 175,600 reads equal the oracle. Its orders never return to an earlier state, though.
- **A-B-A, B late:** all 450 reads of the 150 customers differ from the oracle. Current shows Braavos (B) where the oracle shows Astapor (the second A). The second A wrote no version (`versions written by A 1, B 1, second A 0`). When it arrived, the newest version at its point was the first A, so the store judged it unchanged and wrote nothing. When B landed between them later, nothing went back for it.
- **A-B-A, in order:** all 150 customers equal the oracle.

The history depends on arrival order whenever a later fact repeats an earlier one, which is exactly the case "unchanged writes nothing" is for.

**Also seen:**

- **Floats.** 45 floats in 45 of 36,668 facts were stored one ulp from the value sent; for example, 9.879999999999999 was read as 9.88. Rannoch's `serde_json` is built without `float_roundtrip`. The oracle parses the same text the same way, so this does not count against the edge.
- **Read speed.** Reads averaged 1.96 ms. `state_at` reads through the same `facts()` union whose `_versions` half is now planned on `PRIMARY KEY(line)`.
- **Cursor readers.** The late events land under the committed watermark, so a reader tailing the log by cursor never sees them. The log itself reads in id order.

## Edge 2: same id, different body (revised prediction)

**Prediction.** An exact copy under a logged id is tossed. A different body is stored under a plain new UUIDv7 written into its `id`, the rest of its bytes as sent, and it fills. A re-id'd event delivered again is stored again under another new id.

**Run.** Each step went through `Graph::commit` and through the queue (`accept` then `flush`), with the same results both ways:

| step | log | what happened |
|---|---|---|
| X, then X again | 9 → 9 | `commit` answered the event logged under X's id |
| X′, a different body under X's id | 9 → 10 | stored under a new UUIDv7 (version 7) |
| X′ again | 10 → 11 | stored again under another new id; it made 0 versions |
| X′ under its new id | 11 → 11 | tossed |

For the re-id'd X′:

- its bytes equal X′ with only the id's text replaced;
- the new id is its `CloudEvent` id;
- it made 1 version;
- X's id still holds X's body;
- its known-at is the moment it was re-id'd, 60,007 ms after the original id's.

**Outcome: holds.** The node's current city is X′'s (Myr).

The second copy of X′ is logged but made no version, because it is identical to the first copy's version. The log then holds three events about the node and two of them say the same thing.

## Edge 3: an id from the future

**Prediction.** Accepted, logged at its own time, and read as known from then.

**Run.**

1. A version known now (Pentos) committed.
2. A version whose UUIDv7 is 3,600,000 ms ahead of the clock (Qarth) went through `accept` and `flush`.
3. A version known now again (Volantis) committed.

**Outcome: holds as worded.**

- `accept` gave it ticket 2, and the commit reported no failures.
- It is logged under its own id, last in the log.
- As known now it reads Pentos; as known from its own time it reads Qarth. After Volantis commits, as known now reads Volantis.

What the prediction does not cover:

- Current (`find`) and `state_at` with the default view read Qarth at once, an hour before it is known, and keep reading Qarth after Volantis.
- The committed watermark becomes the future id. A reader at that watermark read 0 events after Volantis committed. For the next hour, every event committed lands under the watermark, unseen by a cursor reader.

## Edge 4: fill cannot apply

**Prediction.** (a) An event of an undeclared class and (b) a storage fault forced during fill both leave the event in the log unfilled, and the commit returns Ok. (b) is the wrong one, since a fault is not an event fill cannot apply.

**Run and outcome: holds through `Graph::commit`; the queue refuses (a) before the log.**

- **(a) Undeclared class:**
  - Through the queue, `accept` refuses it ("class ghost is not declared"), and it never reaches the log.
  - Through `Graph::commit`, beside a good event, the commit returns Ok with both results Ok. The ghost event is in the log with 0 versions.
- **(b) Forced fault:**
  - The fault came from a second connection, which added `CREATE TRIGGER ... BEFORE INSERT ON _versions WHEN NEW.key LIKE 'fault-%' ... RAISE(ABORT, ...)`. It is a SQLite error raised by a trigger (`SQLITE_CONSTRAINT_TRIGGER`), not a disk I/O fault.
  - Through the queue, the flush reports no failures. The fault event is in the log with 0 versions; the good event beside it made 1.
  - Through `Graph::commit` the result is the same: commit Ok, result Ok, 0 versions, and `find` returns None.
  - `fill` and `attempt` in `rannoch-store/src/lib.rs` treat every `Err` from `fill_one` alike, so an I/O error inside the savepoint would take the same path. SQLite may roll back a whole transaction on `SQLITE_IOERR` or `SQLITE_FULL`, and that was not tested.
- **After:** with the trigger dropped, 3 node events in the log have no version. Nothing in the store retries them, and nothing reports them.

As predicted, (b) is the wrong one: a forced fault is swallowed silently, exactly like an event fill cannot apply.

## Edge 5: kill mid-heartbeat, 100 times

**Prediction.** Nothing committed is lost, and no committed event is left unfilled.

**Run.** A child process opened the store, resumed its producer from `position()`, and accepted events through `accept_from`, holding at most 256 past the watermark. It printed every event the watermark had passed, flushing each line. The parent SIGKILLed it at a random moment between its first commit and three times that (344 to 2,554 ms). All 100 kills used one file.

After each kill, the parent reopened the store with rannoch, then checked with SQL:

- every event the child reported committed is in `_events`;
- every node event logged since the last kill has a `_versions` row;
- the events logged this run are exactly `k(resumed+1)` to `k(position)`, each once.

**Outcome: holds.**

- 0 of 100 kills broke a check.
- 39,424 events were reported committed, and 0 were lost.
- No kill left an event unfilled.
- Each run resumed exactly where the last one's committed position stood (0 gaps).
- At the end the file holds 39,680 node events, 39,680 distinct keys and 0 events without a version.
- The 256 events logged but never reported were committed just before a kill; the resume from the position sent none of them twice.

The child saw 154 commits across the 100 runs. Most kills therefore landed while a later commit was in flight; this is an estimate, since the child cannot see a commit start.

## Edge 6: two handles on one file, each writing

**Prediction.** Every id is kept once and the log reads in id order.

**Run.** Two processes ran at once, each with its own handle and queue on one file. Each sent the same 5,000 shared bodies and 20,000 of its own, interleaved, flushing every 200.

**Outcome: fails.**

- Ids are unique, and the log reads in id order.
- All 5,000 shared ids are kept once, each with the body sent, and none was re-id'd.
- But writer b lost 400 of its own events. Two of its batches failed whole: "the batch's transaction failed: the graph file failed: database is locked".
- The log holds 44,600 customer nodes of the 45,000 sent.
- The queue reported these as failures and does not retry them. A producer resumes only from its committed position, and these events were sent without one, so they are gone.

The lock wait is rannoch's own busy handler, 5 seconds. On main, commits on a file of 40,000 versions take longer than that (see basics 1).

At 46e7150 the same run finishes in 1.62 s with 0 failures and all 45,000 nodes kept. The lost writes therefore come from the regression, but the failure mode stays open on any build: a commit that waits longer than 5 s fails its whole batch.

## Edge 7: a store file from before #253 and #256

**Prediction.** Opening it fails or reads wrongly, and existing files are left to a migratr migration (STORE-12).

**Run.** `old/` is rannoch at e81e263 by git. It wrote a file with:

- 13 events in a `seq` log, keyed by a minted id, with `CloudEvent` ids of the form `<uuid>:0`;
- 11 versions;
- fill's place in `_places`, the failed ghost event in `_work`, and the try limit in `_fill`.

A copy of it was opened with main.

**Outcome: fails.** It opens, and every read checked is right.

- `open` returns Ok. The ontology is `customer`.
- `committed()` and `since(START)` read all 13 events, and the log's order by id equals its order by `seq`.
- `find` returns the right current values (c1 "moved", c2 "city-2", c10 "city-10"). `state_at` returns c1's newest version.
- A new commit lands as `seq` 14 and becomes c1's current version.
- Opening and committing added no schema entry. The old columns and tables stay.

What does not carry over:

- An old event delivered again exactly as it was sent is refused ("event id ...:0 is not a UUIDv7").
- All 12 old node events have no version under their log id, because their versions name the uuid inside the `CloudEvent` id. So a log-to-history check such as edge 5's reports them unfilled.
- `_places`, `_work` and `_fill` stay in the file unused. The ghost event's failed fill is never retried or reported.

The file neither fails to open nor reads wrongly, so the claim that existing files need a migration has no evidence here beyond these three gaps.

## What breaks an edge

PREDICTIONS.md names three ways to break an edge. Where each happened:

- **A read that differs from the oracle.** Edge 1, A-B-A with a late middle version (450 of 450 reads). Edge 3 also reads a fact as current an hour before it is known, which no oracle in the predictions covers.
- **A committed event lost or left unfilled.**
  - No committed event was lost in edge 5.
  - Edge 6 lost 400 events at commit, reported as failures, under the 5 s lock wait.
  - Edge 4 leaves events unfilled with an Ok commit, as predicted.
- **A duplicate id that keeps the second body.** Not seen. Edge 2 keeps the first body under the first id and stores the second under a new one.
