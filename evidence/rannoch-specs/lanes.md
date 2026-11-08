# Lanes: spec draft

Draft for the operator's review, 2026-11-07. Nothing is filed or built from it except what the issues below name.

## Source

Operator, 2026-11-07:
- "do we keep the two lanes so things like a purge or something long-running can fan out and not block the railgun events?"
- "lets make these modular so we can switch crates that supply the fast and slow lanes for different shapes, like switch the flat mem queue to a frame ring or change the slow lane to a fullspeed mem queue in huge deploys"
- "I think this a build config."
- "rannoch is very inetional, we don't want runtime config. we will loose precision and speed."
- "na we got to figure this out and n-lane makes more sense now. a slow lane can listen to stdout and read the CE to see if it needs to do anything about it."
- "well, SLOW says yes, right?" (a lane hears an event only once it is committed)
- "less machnery is best, simple." (one log file, no stdout layer)
- "the ring will read before the log... thats where we push to disk speed on sqlite"
- "do we add to a processor what lane it defaults too?"

## Requirements

1. **LANES-1. A fast lane and n slow lanes.** Rannoch SHALL have one fast lane, which takes an event from memory through the worker to one write, and any number of slow lanes. Purges, outside calls, model runs and other long-running work SHALL run in slow lanes and SHALL NOT block the fast lane.
2. **LANES-2. One lane contract.** `rannoch-queue` SHALL be the contract every lane meets, as rannoch-processor is for steps: a lane hands events in order from a position. `rannoch-queue-mem` supplies the fast lane today and `rannoch-queue-trickle`, following the log, a slow lane.
3. **LANES-3. Switched at build.** Which crates supply the lanes, and which CloudEvent types go to which lane, SHALL be chosen in the build configuration. Nothing about lanes is configured at run time.
4. **LANES-4. A processor's default lane.** The processor contract SHALL carry a default lane for each processor, as a constant; the build configuration may override it.
5. **LANES-5. The fast lane reads memory, never the log.** The fast lane SHALL take its events from memory (the in-memory queue, or a frame ring that writes a whole frame per SQLite transaction) and SHALL NOT read the log.
6. **LANES-6. One log, read by slow lanes after commit.** Rannoch SHALL append each event to its own append-only log file once the heartbeat has committed it. Each slow lane SHALL read that file from its place, read each event's type, and act only on the types it handles. There is no stdout layer and no split of the log by lane.
7. **LANES-7. A slow lane is a producer.** A slow lane SHALL send what it produces back through the inbox as events, with its place in the log committed in the same transaction (rannoch-queue: "A producer's position in its source commits in the same transaction as the events it wrote"). A slow lane writes no facts itself.
8. **LANES-8. Long-running work as request and answer.** A long-running step (an outside call, a model run) SHALL be a request event and an answer event carrying a key derived from the causing event; the answer is kept by that key, so a re-sent or replayed request never runs the work twice.
9. **LANES-9. Implementations named by the operator.** Fast lane: the flat memory queue (rannoch-queue today) and a frame ring. Slow lane: a log-following reader and a full-speed memory queue for huge deploys.

## Not now

Lanes that send an event on instead of writing it (a mesh plugs in there) are left out until something needs them.

## Open, for the operator

1. What exactly the lane contract holds.

## Answered

- **A purge's store swap and the fast lane** (purge swap toy, rannoch toys/purge-swap at 78f7fd4, RESULTS.md): rewriting the store without the purged facts and swapping it in kept every write made during the purge (about 61,000), left no purged content in the store, WAL or shm, and changed no surviving read (282,300 of 282,300). The swap held the fast lane 241-263 ms, about one batch (the lane's longest batch without a purge was about 305 ms), and the lane ran at 76-78% of its rate over the purge.
