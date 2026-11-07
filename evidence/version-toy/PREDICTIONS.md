# Version toy: predictions

Written 2026-11-07, before anything is built. It tests the parts of the two-lane architecture (legion evidence/two-lane-toy/ARCHITECTURE.md at 4c4ff073) that research 01a1b50e-49f7-7ac0-b1fa-29c8402b8e5c lists as not proven, so rannoch #222 can be revised from evidence. It extends the two-lane toy (rannoch toys/two-lane at 5579695) and changes only what these predictions name.

## What changes from the two-lane toy

- A changed fact appends a version of its row, carrying the value of its declared axis and the known-at from its event id's clock. That version is its one write.
- Writing a fact again unchanged writes nothing.
- An emitted event's id is a hash of its content and its cause's key, never of which worker or shard answered first.

## The data

Two streams, from fixed seeds:

1. The two-lane toy's Westwind stream and cast, unchanged, to check nothing else moved.
2. A build stream shaped like kessel's (bullpen 01a1b510): 200,000 objects of about 3 KB of JSON each, in 6 builds. Build 1 writes every object; each later build sends every object again, and 2 to 5% of them differ. Build 4 is followed by a fix pass that rewrites 1% of build 4's objects at the same build. The build is the declared axis. No kessel data is used.

## Predictions

1. Builds 2 to 6 write exactly the objects that differ: the count of versions written equals the generator's count of changed objects, and unchanged objects write nothing.
2. Each later build grows the store by no more than 1.2 times the bytes of its changed objects.
3. Build 1 stores at least 5,000 objects a second of 3 KB, at 4,096-event batches, on a quiet machine; the PR records bytes a second beside the disk's durable append rate.
4. An as-of read of any object at any build equals the generator's object at that build, for every object and build, and a single-key as-of read takes under 1 ms at p50.
5. An as-known read of a fix-pass object, as known before the fix, gives the answer from before it; as known after, the fixed one.
6. Neither as-of nor as-known reads touch the log or the hearings.
7. With versions, the Westwind fast lane stays within 10% of the two-lane toy's 32,615-33,636 facts/s at 4,096-event batches, and the cast's outcomes digest stays a8184fc0102a6e76.
8. 10 aborts at random rounds, on both streams, give the same versions, emitted ids and outcomes as an unkilled run.
9. Four workers give the same set of emitted ids and the same rows as one worker, on the Westwind chain.
10. The log, carrying 3 KB bodies, costs the build stream's main path under 10% at 4,096-event batches.

## What breaks the idea

- An as-of or as-known read needs the log or the hearings.
- An unchanged object costs disk.
- Versions cost the Westwind fast lane more than 25%.
- Emitted ids still differ between one worker and four.
