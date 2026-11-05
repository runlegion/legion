# Worker toy: predictions

Written 2026-11-05, before anything is built. The toy runs on rannoch v0.5.0 plus #213 (known-at from the event clock, a declared axis), outside the rannoch crates.

## The claim

An event's type alone decides where it goes: to a built-in or to a flow stack. Handlers are supervised the way OTP supervises processes, and a flow rebuilds the way Temporal rebuilds a workflow: by replaying its history.

## The guarantee

Every event reaches each handler its type routes to at least once. Getting it again changes nothing, because applying an event is deterministic: its identity is its content, source and position, and its times are inside it. What a flow does with an event beyond that, including emitting more, is the flow's.

## What runs

1. One entrance: an event's attributes are read once, and it travels as type, source, subject and body.
2. A router: a table from type pattern to handlers, built from what handlers register.
3. A supervisor per handler, one_for_one: a crash restarts that handler from its place; an event that keeps failing goes to that handler's bin and the rest carry on.
4. Built-ins: store (v0.5.0 fill applied to one routed event) and declare.
5. A flow runner, ported from the 2026-10-21 subs toy: JSONL rows of guard, step and next or else. Each step's result is an event, so an instance rebuilds by replaying its history. An instance is keyed by a correlation the flow declares, or one instance when it declares none.
6. Steps: emit, host (an action outside the store), after (a timer on event time) and store.

## The data

legion's live memory, read only: about 27,000 reflections enter as `legion.raw.reflection`. One flow guards that type, calls a host classifier (the rules from the ETL kinds toy), and emits `memory.<kind>`, which routes to the store built-in. An after row rechecks what stays unclassified.

## Predictions

1. One attribute read per event, counted.
2. Routing costs under 1 microsecond per event.
3. A flow with no store step runs at 500,000 events a second or more; the full path is bounded by store at about fill's speed, near 80,000 a second on a quiet machine.
4. Store through the router is no more than 20% slower than v0.5.0's fill on the same events.
5. The same log replayed 1, 10 and 1,000 times, in order and shuffled, and killed at random, leaves the store byte-identical every time; a flow instance rebuilt from its history equals one that ran without a stop.
6. Timers count from event time, so a replay fires the same timers in the same order.

## What breaks the idea

- Store through the router more than 20% slower than the hard-wired fill.
- Two replays of the same log that leave different stores.
- Rebuilding an instance gets slower without bound as its history grows.
