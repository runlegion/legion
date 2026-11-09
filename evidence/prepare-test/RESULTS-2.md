# Prepare test, round 2: results

Run 2026-11-09 against ANSWERS-2.md, committed before any agent ran. Five `legion:rusty` agents (plugin 0.44.9, no memory step in its definition), each given only a need in plain words: no issue number, no mechanism, no trace.

## Scores

| Need | Result | Where the answer came from | What went wrong |
|---|---|---|---|
| 1. Raw rows gone | Partial | The open #170 and its park comment (the trap) | Missed trash, the cleaner, STORE-10 and the app guide; proposed a processor that deletes `_events` rows, which breaks append-only and "no trim"; claimed #210 was "merged as 6dea266", which is false |
| 2. Faster fast lane | Pass | Reflection 01a1c12c (ring parked to 125,000/s), the two-lane toy, events-only.md | Nothing; it also found the out-path cost below |
| 3. Crash between take and finish | Partial | Code (`_work`, `hand_out`) and recall | Found events-only but treated it as later; called the need "already built" on the bookkeeping the design cuts; cited a stale reflection of legion's (01a1c143, since removed) that called duplicates a gap |
| 4. Renamed key | Pass | store-core.md STORE-9 | Nothing |
| 5. Faster writes | Pass | The graph-vs-relational and chain toys, #238, reflections | Nothing; recommends measuring main after #238 first |

Three passes, two partials, no fails.

## What it shows

- Given only the need, the builder reasons from the record in three of five: it found a spec (STORE-9), a decision in memory (the ring's threshold) and two toys' results, all unprompted.
- It goes wrong where the code or an open issue looks like the answer: #170 for need 1, `_work` for need 3. When the record and the code disagree, it trusts the code or the issue.
- Memory with contradictions misleads it: need 3 quoted legion's own superseded reflection. Two such reflections were removed during the run (01a1c143, 01a1c14c).

## The test leaked

The answer keys sit in legion's evidence folder, which is indexed. Need 5 reported that a search returned ANSWERS-2.md and that it did not open it; the others did not say. Need 4's pass came from store-core.md, the right source, but a look at the key cannot be ruled out. Next round: commit only a hash of the key before the run, and the key itself after scoring.

## Found along the way (unverified by legion)

- Need 2: the fast lane's limit may be on the way out: `hand_out` and `done` each make a durable write per event (rannoch-store inbox.rs:80 and :122), estimated at 150 to 170 events a second whatever the intake. The same path reads `_events`, against LANES-5. The events-only surgery removes both.
- Need 5: main may still store a fact's content three times after #238 (the `_events` body, `_versions.properties` at history.rs:232, the class table at lib.rs:530), and each heartbeat makes two durable transactions (commit, then fill). Write speed has not been measured since #238.
- Need 4: docs/architecture.md still describes renames and `_renames` as built; main has neither.
