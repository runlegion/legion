# Two-lane toy: predictions

Written 2026-11-07, before anything is built, after the chain toy (rannoch toys/chain a08ed4d) proved the chain of flows and found rannoch's per-record write cost dominating: every fact written three times (log body, hearing copy, class table) under five indexes, about 5,000 to 7,500 records a second against SQLite's 77,688 rows a second on the same Westwind rows.

## The shape

The design as drawn in conversation on 2026-11-07:

```
cloud event -> inbox -> worker -> one write (the row) -> load the flows for that row
                 ^                                            |
                 +---- independent steps, as new cloud events +
                       dependent steps wait inside the flow, by clock or by dependency
log: a stream (append-only file), never on the main path -> trickle reader -> hearings, history
```

- The inbox is durable and takes CloudEvents.
- The worker decides what it does now and what it leaves to flows. The toy's workers write first and leave the rest to flows.
- One write per fact: the row as it is now, with the id of the event that made it. No log table, no hearing table, no copy of the row on the main path.
- After the write, the flows registered for that row's type load and run. Independent steps go back on the inbox as new CloudEvents. A step that depends on another waits.
- The row is the flow's state. No flow instance is stored apart from the row.
- Each flow declares how its events are ordered: by clock (event time, with a lateness), by dependency (an event is ready when the events it needs exist), or neither.
- A timer is a CloudEvent on the inbox for delivery at a later event time.
- The log is a plain append-only stream written beside the main path. A trickle reader processes it afterwards into hearings (who said what, when) and history.
- What a type does to a row is the worker's code. Rannoch-side code knows only how to hold an event until its flow's ordering condition is met.

The store here is the toy's own, on SQLite, built to this shape. It is not rannoch's current store, whose write path is what this toy tests an alternative to.

## The data

The chain toy's event-shaped Westwind stream, its cast (Cersei, Jon Snow, Davos, Tyrion, Arya, Daenerys, Hodor, the Lion, the Tin Man, the Scarecrow) and Oz beside it, with the same expected trails. For speed, the chain toy's extended orders (400,839 orders).

## Predictions

1. Every cast trail and every seeded order matches the chain toy's expected answers (13 of 13 trails, 320 of 320 orders).
2. The fast lane (inbox to written row) stores at least 40,000 facts a second on the extended orders, on a quiet machine, with the inbox durable (fsync per batch).
3. The graph's file, without the log, is no more than 2 times relational's 471 MB on the same data.
4. Writing the log beside the fast lane costs it under 5%: the fast lane with the log on is within 5% of the log off.
5. The trickle reader keeps up: it processes the log at least as fast as the fast lane writes it, so its lag stays bounded, and the hearings it builds answer who said what and when for every cast event.
6. No flow state lives outside the row: killing the process at random points, 10 times, gives the same rows, the same emitted events and the same outcomes as a run with no kill.
7. Ordering per flow: a clock-ordered flow (stock), a dependency-ordered flow (ship waits for pack, approval waits for its request) and an unordered one (independent lines) each give the same outcomes under three shuffles of the input as in order.
8. Fan-out runs in parallel: with independent steps as CloudEvents on the inbox, 4 workers process the stream at least 1.5 times faster than 1 worker, with the same outcomes.
9. Timers as future CloudEvents: the Lion's open ends close, and Tyrion's 10 years of monthly orders arrive, at their event times.
10. A duplicate event id changes nothing, and a host step's answer is kept by event id durably, so a kill never makes a host step act twice (Cersei's return is authorized once).

## What breaks the idea

- The fast lane has to read the log, or a hearing, to write a row.
- A flow needs state that the row cannot hold.
- A shuffle gives different outcomes for a flow whose declared ordering covers it.
- The fast lane stays under 10,000 facts a second.
