# Events only: plan

Draft for the operator's review, 2026-11-09. Nothing is filed or built from it. Two parts, in order: the surgery, then the toys that prove the shape. Checked against rannoch main 9ffb233 with legion sym. Operator's words are quoted; everything else is legion's and unconfirmed.

## The queue

```
_events        (seq, id, type, body)     appended once, never changed
_event_status  (seq, status, known_at)   appended: 2 processed, -1 failed (+ reason)
_positions     (producer, position)      a producer's place in its outside source
```

- **In.** `accept` checks a CE against the ontology and holds it; the heartbeat commits the batch to `_events`. Duplicates are allowed: "we can have dups in the event queue we are not going to stop and look". A repeated fact writes nothing (STORE-1), and steps are idempotent.
- **Out.** A lane takes the next event of its types with no status row, runs it, and in one heartbeat transaction appends the CEs it produced and the event's status. "CE->inbox->procesor::<kind>->CE->graph <> CE for more work": only the graph writes facts.
- **Status.** One per event, an int "for speed": 0 is the `_events` row; in progress is kept nowhere; 2 and -1 are appended rows ("then it's temporal"). An event is handled once; when two flows need the same thing, the step emits one event for each.
- **Crash.** An event with no 2 row and fewer than the limit of -1 rows runs again.
- **Retries.** "-1 and time? retry x times per x time": each failed attempt appends a -1 row; attempts are their count; dead is the limit. Defaults: 10 attempts inside a 20-minute total window, exponential backoff, then dead; a lane moves past an event waiting out its backoff. The policy is in the settings file ("make the policy settings in the settings file"), read at build.
- **The bin** is part of the queue ("bin should not be a processor"): the dead events, found by their -1 rows.

## Flows

- A step is a CE typed `flow.<flow file>.<row id>` ("flow.<name>.step_5"; a row id, so editing a file does not misroute events in flight).
- "the row has all the state data, since this temporal, we can replay the flow": a step's data carries the instance's whole state. Flow state is not graph data ("its transitent"); the events are kept ("IT IS TEMPORAL"); every step is durable.
- Flow files are JSON so they "can be added/edited without a build": read at run time, each parsed once when loaded, never per event. Lanes and retry policy are fixed at build; flows are not.
- `rannoch-processor` is "the generic for rannoch-processors-* only": the contract each step kind meets, CE and row in, CEs out.
- Store stays direct: `rannoch.node` and `rannoch.link` go to fill.
- Rust code that handles its own events needs no flow: it reads its types from a lane.

## 1. Surgery

Scope: "everything." Nothing outside rannoch depends on these (kessel and eavesdrop reference rannoch only from their toys).

| Cut | Where (9ffb233) | Replaced by |
|---|---|---|
| `done`, `failed`, `State` on the lane contract | `rannoch-queue/src/lib.rs` | Status rows |
| `hand_out`, `_places`, `_work`, `handed`, `processed`, `remove_worker`, `in_progress` | `rannoch-store/src/inbox.rs` | The next event of a lane's types with no status row |
| Fill's worker place (`rannoch.fill`), its failed rows, `_fill` try limit, its retries | `rannoch-store/src/lib.rs` | Status rows and the retry policy |
| `take_failures`, `on_failure`, `processed`, `remove_worker`, `state` passthroughs | `rannoch-queue-mem/src/lib.rs` | Nothing; `Failure::Event` at commit stays (it is accept's) |
| `done`, `failed` on `Trickle` | `rannoch-queue-trickle/src/lib.rs` | Status rows |
| `rannoch-processor-bin`, `rannoch-harness` | `crates/` | The queue, and the lanes running rows |

Added: `_event_status`, and the retry policy in the settings file.

Kept: `_events` with its type column and index (#243), `_positions`, `accept`, `accept_from`, the heartbeat, the ontology check at accept, the core store (#238), `Trickle<T: Types>`, `rannoch-processor` and the `rannoch-processor-*` step kinds, `rannoch-worker`.

Docs rewritten to match: `docs/flows.md`, `docs/inbox.md`, `docs/architecture.md`, lanes.md.

Done when a kill test shows every committed event without a 2 row runs again and nothing committed is lost.

## 2. Toys

Predictions are written and committed here before any code.

**T1, flows as events.** Westwind, on rannoch after the surgery. Three flows: a chain (host, then emit), a `count` by key, and a `wait` for a matching event, on the fast and a slow lane. Random kills; checked against SQL over the same seed. Measures: the graph equals the oracle; a poison event is dead after 10 attempts; facts a second against today's fill and the two-lane toy's 33,000; log rows and bytes per fact; whether a lane's next event and a parked `wait` are found by a seek (`EXPLAIN QUERY PLAN`).

**T2, a direct Rust reader.** T1's chain as Rust on a lane, with no flow. Measures facts a second and log rows per fact against T1: the cost of declaring a step as data.
