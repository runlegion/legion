# Prepare test: answer key

Written 2026-11-09, before any agent ran. The operator asked for "five little tests": real open rannoch issues where the record (reflections, specs, toys) already decides what should happen, so a builder that solves the issue as if no one had thought about it before goes wrong. Each run gives the current `legion:rusty` (plugin 0.44.9, which has no step that reads memory) one issue and asks it to prepare only: look first, place, predict, and report its findings, with no code, branch or prediction recorded.

Rannoch main is at 9ffb233. A run passes when its findings reach the answer below and cite at least one of the sources listed.

## 1. rannoch #170, Trim: retention declared per class

**Answer: do not build.** The operator decided "no trim" (store-core.md STORE-10; app-guide.md item 6; trash.md "Rannoch offers no trim"). Retention is trash plus the cleaner on the trickle lane, with the rules in the app's code.
Sources: legion evidence/rannoch-specs/store-core.md, trash.md, app-guide.md; reflections on "no trim".

## 2. rannoch #242, the frame ring for the fast lane

**Answer: do not build now.** The operator parked it on 2026-11-09 until something needs more than 125,000 writes a second; rannoch-queue-mem stays the fast lane.
Sources: reflection 01a1c12c; lanes.md LANES-5 and LANES-9 name the ring, which is the trap.

## 3. rannoch #209, route by type

**Answer: mostly exists already.** `_events` has a type column with an index, `Graph::next_of` hands a worker only its types (#243), and `Trickle<T: Types>` reads by type (#241). Routing for flows is the events-only plan's `flow.<file>.<row>` type. What #209 asks is built or superseded.
Sources: rannoch-store inbox.rs:66, rannoch-queue-trickle lib.rs:12, legion evidence/rannoch-specs/events-only.md.

## 4. rannoch #206, purge arrives as a CloudEvent

**Answer: do not build; superseded.** #210 removes the purge code, and the trash spec makes purge the cleaner's job on the slow lane, driven by the app's rules. The purge-swap toy proved a purge's store swap holds the fast lane only about one batch.
Sources: rannoch #210, legion evidence/rannoch-specs/trash.md, legion evidence/purge-swap-toy, lanes.md "Answered".

## 5. rannoch #210, remove purge (the control)

**Answer: build it.** The issue still holds: the special purge code goes, and purge returns later as the cleaner (trash.md). A run that refuses to build this one, or invents a reason to keep purge, fails the control.
Sources: rannoch #210, trash.md.
