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
5. A flow runner, written fresh from the design, reusing no earlier toy or PR: JSONL rows of guard, step and next or else. Each step's result is an event, so an instance rebuilds by replaying its history. An instance is keyed by a correlation the flow declares, or one instance when it declares none.
6. Steps: emit, host (an action outside the store), after (a timer on event time) and store.

## The data

Westwind, the team's shared sample database (research document 01a1b37a-e007-75e1-8cbc-8f029c6d397a), rebuilt from its generator and checked against its seed hash. Each row enters as a typed CloudEvent: `westwind.customer.joined`, `westwind.product.listed`, `westwind.order.placed`, `westwind.order_line.added` and so on. Built-in store takes them into a Westwind ontology that declares the relations Westwind's tables leave implicit, `employees.reports_to` included.

One flow guards `westwind.order_line.added`: a host step looks up the product's stock, emits `westwind.stock.low` when a line takes it under its reorder level, and an after row emits `westwind.order.unshipped` for an order still without a shipped date a set time after it was placed, by event time. Their answers are checked against SQL over the same seed.

For the speed predictions, a generator in the toy extends the order history from Westwind's customers and products with a fixed seed, to about 1,000,000 order lines, so every run sees the same events.

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
