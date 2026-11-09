# Events only: plan

Draft for the operator's review, 2026-11-09. Nothing is filed or built from it. Two parts, in order: surgery on what the last session added, then the toys that prove the shape. Checked against rannoch main 9ffb233 with legion sym.

## The shape

Operator and legion, 2026-11-09. Not built, not measured.

- `CE -> inbox -> step -> CE -> graph`, and the graph emits CEs for more work. Nothing but the graph writes facts.
- A flow step is a CE typed `flow.<flow file>.<row id>`. Its data carries the instance's whole state, so flow state lives nowhere else and a flow replays from its events (operator: "the row has all the state data, since this temporal, we can replay the flow").
- No processor component. A lane reads its types from a position, runs the row each `flow.*` event names, and commits the CEs it emits together with its position. A step kind (host, emit, wait, count, branch, ...) is a function: CE and row in, CEs out.
- Flow files are read at build and become Rust; at run time a lane matches on the type column and hands the body to the step as borrowed bytes. Only a guard or template that reads a field parses `data`.
- Rust code that handles its own events needs no flow: it reads its types from a lane and commits its position with what it emits.
- Store stays direct: `rannoch.node` and `rannoch.link` go to fill, outside the flow namespace.

## 1. Surgery

What the shape makes unneeded is the bookkeeping that covers a place saved apart from its effects. Nothing outside rannoch depends on it (kessel and eavesdrop reference rannoch only from their toys).

| Cut | Where (9ffb233) | Added | Why it goes |
|---|---|---|---|
| `done`, `failed` on the lane contract; `State` | `rannoch-queue/src/lib.rs:63,69` | #237 | A lane's progress is its position, committed with what it emitted |
| `hand_out` writing `_places` and `_work` on every read; `handed`; re-handing in-progress events | `rannoch-store/src/inbox.rs:80` | before #237; `next_of` (#243) reuses it | `next` and `next_of` become reads from a given position that write nothing |
| `_places`, `_work`, `processed`, `remove_worker`, `in_progress` | `rannoch-store/src/inbox.rs` | before #237 | A reader's place is one row in `_positions` |
| Fill's worker place (`rannoch.fill`), its failed rows, `_fill` try limit, retries | `rannoch-store/src/lib.rs` (FILL, 19 refs) | before #237 | Fill keeps a position; an event it cannot apply becomes a bin CE with its reason |
| `take_failures`, `on_failure`, `processed`, `remove_worker`, `state` passthroughs | `rannoch-queue-mem/src/lib.rs` | before #237 | They pass through the cuts above; `Failure::Event` at commit stays (it is accept's) |
| `done`, `failed` on `Trickle` | `rannoch-queue-trickle/src/lib.rs:44,48` | #241 | As the contract |
| `rannoch-worker`, `rannoch-harness`, `rannoch-processor`, eleven `rannoch-processor-*` crates | `crates/` | scaffolds | Empty, and the shape has no processor or harness; the step-function crate is named after the toys |

Kept: `_events` and its type column and index (#243), `_positions`, `accept` and `accept_from`, the heartbeat, the ontology check at accept, the core store (#238), `Trickle<T: Types>` reading by type (#241).

Docs rewritten to match: `docs/flows.md`, `docs/inbox.md`, `docs/architecture.md`; lanes.md LANES-2 (the contract), LANES-4 (a step kind's default lane), LANES-7.

The cuts remove crash recovery and put positions in its place, so the surgery is not done until a kill test shows a reader resumes from its position and loses nothing committed.

## 2. Toys

Predictions are written and committed here before any code, as with every toy.

**T1, flows as events.** Westwind, on rannoch after the surgery, outside the rannoch crates. Flow files compiled at build. Three flows: a chain (host, then emit), a `count` by key, and a `wait` for a matching event; each with a row on the fast lane and a row on a slow lane. Random kills. Checked against an oracle in SQL over the same seed. To measure:
- The graph after kills equals the oracle, and replaying a flow from its events writes nothing new (content-hash CE ids).
- A poison event lands in the bin once, and the position moves past it.
- Facts a second, against today's fill path and the two-lane toy's 33,000.
- Log rows and bytes per fact, and what finished flows leave in `_events`.

**T2, a direct Rust reader.** The same Westwind work as T1's chain, written as Rust on a lane with no flow and its own parsing. To measure: facts a second and log rows per fact against T1, so the cost of declaring a step as data is known.

## Open, for the operator

1. What happens to a finished flow's events in a log that keeps everything: kept, purged by the app's rule, or written to a memory lane that never reaches `_events`. T1's log measurement informs it.
2. Every step durable, or only a change of lane or a wait.
3. Fill's failures: does a bin CE replace retries, or does fill keep a retry before the bin.
