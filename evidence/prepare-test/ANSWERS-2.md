# Prepare test, round 2: answer key

Written 2026-11-09, before any agent ran. Round 1 (RESULTS.md) was too easy: the issues carried their own decisions in park comments and traces. Operator: "try to remove all those choices from the issue and only have the what and see if they can reason". Each brief here states only a need in plain words: no issue number, no mechanism, no trace. The decision for each is on record only in memory, legion evidence/rannoch-specs, or a toy, not in rannoch's code.

Same agent as round 1: `legion:rusty`, plugin 0.44.9, read only, steps 1 to 3, report. A run passes when it reaches the answer and cites a source for it. Also noted: whether it found a matching GitHub issue and leaned on it.

## 1. Raw rows gone after processing

Need: "An app imports raw data into rannoch and has no further use for the raw rows once they are processed. It wants them gone."
Answer: no trim in rannoch. The app trashes what it wants gone and its own rules purge it later, through the cleaner on the trickle lane.
Sources: store-core.md STORE-10, trash.md, app-guide.md items 5 and 6. Trap: rannoch #170 (trim) is still open; it carries a park comment.

## 2. A faster fast lane

Need: "rannoch's fast lane should be faster."
Answer: do not build a new lane now. The frame ring is parked until something needs more than 125,000 writes a second (operator, 2026-11-09); the two-lane toy measured 33,000 to 49,000 facts a second, bound by disk syncs, not by the queue.
Sources: reflection 01a1c12c; two-lane research 01a1b50e and reflection 01a1b50a. Trap: lanes.md LANES-5 and LANES-9 name the ring, and rannoch #242 exists.

## 3. Lost or repeated work after a crash

Need: "If rannoch crashes after a worker takes an event and before it finishes, work can be lost or done twice."
Answer: the events-only design: no in-progress state at all; each event gets an appended status row (2 processed, -1 failed) in the heartbeat transaction with what it produced; anything without one runs again; duplicates are allowed and steps are idempotent. The current `_work` rows are to be cut, not extended.
Sources: legion evidence/rannoch-specs/events-only.md; reflections 01a1c143, 01a1c15a, 01a1c165. Trap: `_work` and `hand_out` exist in code and look like the place to add to.

## 4. A renamed key keeps its history

Need: "An app renames a node's key and wants the node's history to follow it to the new key."
Answer: no rekey. In a temporal store a changed key is the old key trashed and the new key written, and history keeps both (operator, 2026-11-08).
Sources: store-core.md STORE-9. Trap: the first edition had renames and `_renames`; architecture.md still describes them.

## 5. Faster writes

Need: "rannoch stores about 5,000 to 7,500 records a second where plain SQLite stores about 77,000 on the same rows. Make writes faster."
Answer: the cause was three writes per fact under five indexes; reshaping data into larger aggregates made it worse (1,122 orders a second against 1,712); the answer is one write per fact, which the core store (#238) built. The next step is to measure main after #238, not to build.
Sources: reflection 01a1b47d (graph-vs-relational and chain toys), two-lane research 01a1b50e, store-core.md STORE-1, rannoch #238.
