# Lanes: spec draft

Draft for the operator's review, 2026-11-07. Nothing is filed or built from it.

## Source

Operator, 2026-11-07:
- "do we keep the two lanes so things like a purge or something long-running can fan out and not block the railgun events?"
- "lets make these modular so we can switch crates that supply the fast and slow lanes for different shapes, like switch the flat mem queue to a frame ring or change the slow lane to a fullspeed mem queue in huge deploys"
- "I think this a build config."
- "it would not be hard to make this n-lanes now, this could push up to kafka speed if someone wanted too if we allow n-lanes. We don't need to add mesh code ourselves but events could accept and send not accept and write"

## Requirements

1. **LANES-1. Two lanes.** Rannoch SHALL have a fast lane, which takes an event through the inbox, the worker and one write, and a slow lane, which follows the timeline off the fast lane. Purges and other long-running work SHALL run in the slow lane and SHALL NOT block the fast lane.
2. **LANES-2. One lane contract.** `rannoch-queue` SHALL be the contract every lane meets, as rannoch-processor is for steps: a lane hands events in order from a position. `rannoch-queue-mem` supplies the fast lane and `rannoch-queue-trickle`, following the log, the slow lane.
3. **LANES-3. Switched at build.** Which crates supply the fast and slow lanes, and which CloudEvent types go to which lane, SHALL be chosen in the build configuration. Nothing about lanes is configured at run time (operator: "rannoch is very inetional, we don't want runtime config. we will loose precision and speed").
4. **LANES-4. Implementations named by the operator.** Fast lane: the flat memory queue (rannoch-queue today) and a frame ring. Slow lane: a log-following reader and a full-speed memory queue for huge deploys.

## Not now

n lanes, and lanes that send an event on instead of writing it, are left out until something needs them; the lane contract leaves room for both (operator: "I just made this stupid complex when we don't need it").

## Open, for the operator

1. What exactly the lane contract holds.
2. Whether the slow lane's swap of a rewritten store (for a purge) can happen without pausing the fast lane, given SQLite's single writer.
