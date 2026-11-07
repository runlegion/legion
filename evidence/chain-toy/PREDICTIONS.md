# Chain toy: predictions

Written 2026-11-07, before anything is built, after the worker toy's replays. It runs on the worker toy's router, supervisors and flow runner, rebuilt where the worker toy's findings require it, on rannoch main, outside the rannoch crates.

## The claim

A whole service runs as a chain of flows, each firing on the type of an event an earlier step emitted, from an order placed to its delivery and after. Several ontologies share one store, and an event never lands in an ontology it does not belong to.

## The data

Westwind's world (research document 01a1b37a-e007-75e1-8cbc-8f029c6d397a: its customers, products, suppliers, employees and shippers), rebuilt to fit the events, not the schema. The graph-vs-relational toy showed that loading Westwind's tables row for row costs about 10 times what SQLite pays, on rannoch main and on v0.6.0 alike. So the seeded generator emits what happens in the business, each event one thing that happened carrying what it is about: `order.placed` with its lines inside, `order.shipped` with its shipment, `ticket.opened` with its text. No event exists to carry a row, and nothing is a node because it was a table: an order is a node with its lines, and edges join it to its customer and to the products it holds. Support tickets, returns, refunds, shipments with tracking, made-to-order products and supplier orders are in the same stream, all keyed by UUIDv7. A second, small ontology, Oz, sits beside it in the same store.

The 320 seeded orders take the plain path. A cast of scripted customers takes every other path, each with a known event trail:

| Customer | Path |
|---|---|
| Cersei | made to order, late delivery, wrong item, return, a person approves the remake, compensation |
| Jon Snow | large fan-out, partial backorders, split shipments, an address change mid-shipment |
| Davos | a perishable with a spoil-by timer, loss in transit |
| Tyrion | a monthly subscription: a timer places the next order, for years |
| Arya | cancellation at every stage, compensated before shipping |
| Daenerys | an open-ended pre-order |
| Hodor | the same delivery event twice (one `id`), and two identical tickets (two `id`s) |
| Lion | abandoned orders, an approval nobody answers, a return never sent |
| Tin Man | a handler that stalls mid-step |
| Scarecrow | valid events from the wrong universe: an Oz source and customer sent at Westwind |

Support tickets carry free text; a host step triages each into a fixed list: remake, refund, address change, lost in transit, noise.

## The chain

accept, allocate, pick and pack (a join on every line), ship, deliver, after-ship, support, returns. Each stage is a flow file; none names the next stage, only the type it emits.

## Predictions

1. Every cast member's trail matches its expected events in order, rebuilt from the log alone through each event's causation link back to `order.placed`.
2. The seeded orders' outcomes match SQL over the same seed.
3. Every emitted event is a valid CloudEvent 1.0, and every type's data passes its schema.
4. Multiple ontologies: 0 events land in an ontology they do not belong to. Every Scarecrow event, and every event sent across from Oz to Westwind or back, goes to the bin with the reason that it belongs to another domain, distinct from the reason for a malformed event. Oz's own events land in Oz.
5. Checking an event's domain costs under 100 ns per event.
6. A duplicate `id` changes nothing anywhere: store, flow state or alerts. Two identical tickets with different `id`s are two tickets.
7. The Tin Man's stalled handler is restarted by its supervisor, his event is handed again, and the work happens once.
8. The Lion's open ends all close by timer, by event time.
9. Late events: a flow declares how late an event may arrive, and does not act on a timer until the events it waits on can no longer arrive in time. A shuffled replay within that lateness gives the same alerts and outcomes as an in-order one. The worker toy's shuffled replays reported 90 to 129 unshipped orders against 37.
10. Tyrion's subscription, run for 10 years of monthly orders, rebuilds in the same time in year 10 as in year 1: an instance carries its state into a fresh history rather than growing one without end. The worker toy measured rebuild growing linearly, about 2 us per history entry.
11. Everything a step sends is in its history, and a history entry is durable only after the store writes it records. Both broke in the worker toy and were caught by replay.
12. The full chain over the seeded orders and the cast, replayed 1, 10 and 1,000 times, stopped and killed, leaves the same store contents and the same flow states, comparing all but each event's arrival time (`recorded`).
13. Loading the event-shaped stream stores an order at least 3 times faster than the row-for-row load did on the same main (7,493 rows/s, an order and its lines counted as one order), on a quiet machine, and in less than half the bytes per order.

## What breaks the idea

- An event lands in another ontology's graph.
- A stage has to name the stage after it for the chain to run.
- A shuffled replay within the declared lateness gives different outcomes from an in-order one.
- Rebuilding a long-lived instance still grows with its age.
