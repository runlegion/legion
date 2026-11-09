# Events only: plan

Draft for the operator's review, 2026-11-09. Nothing is filed or built from it. Two parts, in order: surgery on what the last session added, then the toys that prove the shape. Checked against rannoch main 9ffb233 with legion sym.

## The shape

2026-11-09. Not built, not measured. Each point says who said it.

Operator said:
- "CE->inbox->procesor::<kind>->CE->graph <> CE for more work": only the graph writes facts, and it emits CEs for more work.
- Flow state is not graph data: "it's not 'finished' data. its transitent ... if we crash the flow starts over and and it harms nothing".
- "the kind type can be flow.<name_of_flow_flile_to_read>", then "flow.<name>.step_5".
- "the row has all the state data, since this temporal, we can replay the flow": a step's CE data carries the instance's whole state, and a flow replays from its events.
- "do we need a processor at all? the CE is the producer."
- Parsing every flow's JSON per event is not acceptable when Rust code handles its own events outside the queue.

Legion proposed, not confirmed:
- The type names a row id, not a step number (`flow.<file>.<row id>`), so editing a file does not misroute events in flight.
- No runner component: a lane reads its types from a position, runs the row each `flow.*` event names, and commits the CEs it emits with its position.

Operator said (2026-11-09), on an event's state: "the event should have -1|0|1|2 for it's 'status'", stored as an int "for speed". Read as -1 binned, 0 added, 1 in progress, 2 processed (legion's reading, to confirm). Then: "then it's temporal" (a status change is appended, never updated, as STORE-2), and "we don't need to do in-progress events, those can goto paper log". So: 0 is the `_events` row existing; 1 is not kept anywhere (the paper log was the stdout log LANES-6 cut; operator: "if it's cut. leave it cut. flows don't log I guess"); 2 and -1 are each one row appended to `_event_status (seq, status, known_at)`, -1 with its reason, in the heartbeat transaction. The body never changes. Legion proposed, not confirmed: one reader per type; a reader keeps in memory the lowest event it has not processed, derived from `_event_status` at start, so finding its next event does not walk every processed one.

Retries, operator 2026-11-09: "-1 and time? retry x times per x time"; "make the policy settings in the settings file". Each failed attempt appends a -1 row with its time and reason; attempts are the count of -1 rows; dead (the bin) is N of them. Legion proposed, not confirmed: backoff counts from the last failure, on wall time; whether a reader moves past an event waiting out its backoff is a setting (unordered flows skip ahead, ordered ones wait). The settings file is the build configuration LANES-3 names, which does not exist yet; legion proposes `rannoch.toml` at the app crate's root, read by `build.rs` into constants: the lanes (crate and types), and the retry policy (attempts, backoff, skip-ahead) as a default with overrides by type.

Operator said (2026-11-09): `rannoch-processor` is "the generic for rannoch-processors-* only": the contract each step kind meets, not a runner. A step kind (`rannoch-processor-host`, `-emit`, `-wait`, ...) meets it: CE and row in, CEs out.
- A step reads the body as borrowed bytes; only a guard or template that reads a field parses `data`.
- Rust code that handles its own events needs no flow: it reads its types from a lane.
- Store stays direct: `rannoch.node` and `rannoch.link` go to fill, outside the flow namespace.

## 1. Surgery

What the shape makes unneeded is the bookkeeping that covers a place saved apart from its effects. Nothing outside rannoch depends on it (kessel and eavesdrop reference rannoch only from their toys).

Scope is open: the operator asked to cut "the fat that crept in last session". Only the rows marked #237, #241 or #243 came from that session (2026-11-07 and 11-08); the rows marked "before #237" are older and are listed because the shape makes them unneeded too.

| Cut | Where (9ffb233) | Added | Why it goes |
|---|---|---|---|
| `done`, `failed` on the lane contract; `State` | `rannoch-queue/src/lib.rs:63,69` | #237 | A lane's progress is its position, committed with what it emitted |
| `hand_out` writing `_places` and `_work` on every read; `handed`; re-handing in-progress events | `rannoch-store/src/inbox.rs:80` | before #237; `next_of` (#243) reuses it | `next` and `next_of` become reads from a given position that write nothing |
| `_places`, `_work`, `processed`, `remove_worker`, `in_progress` | `rannoch-store/src/inbox.rs` | before #237 | A reader's place is one row in `_positions` |
| Fill's worker place (`rannoch.fill`), its failed rows, `_fill` try limit, retries | `rannoch-store/src/lib.rs` (FILL, 19 refs) | before #237 | Fill keeps a position; an event it cannot apply becomes a bin CE with its reason |
| `take_failures`, `on_failure`, `processed`, `remove_worker`, `state` passthroughs | `rannoch-queue-mem/src/lib.rs` | before #237 | They pass through the cuts above; `Failure::Event` at commit stays (it is accept's) |
| `done`, `failed` on `Trickle` | `rannoch-queue-trickle/src/lib.rs:44,48` | #241 | As the contract |
| `rannoch-processor-bin` | `crates/` | scaffold | Operator, 2026-11-09: "bin should not be a processor ... it's part of the queue". A lane that fails an event bins it and moves its position past it, in one commit; a row's `else: bin` fails the event to the queue |
| `rannoch-harness` | `crates/` | scaffold | Empty, and the lanes run the rows; open whether it is kept as the place lanes call rows from |

Added, because the cuts need them:
- A commit of a position alone, for a reader that handles an event and emits nothing (`accept_from` commits a position only with events).
- A read cursor each reader holds in memory ahead of its committed position, so it does not re-read events between heartbeats.

Kept: `_events` and its type column and index (#243), `_positions`, `accept` and `accept_from`, the heartbeat, the ontology check at accept, the core store (#238), `Trickle<T: Types>` reading by type (#241), `rannoch-processor` (the step-kind contract) and the `rannoch-processor-*` step kinds, `rannoch-worker` (reading a flow file and checking it against the ontology).

Docs rewritten to match: `docs/flows.md`, `docs/inbox.md`, `docs/architecture.md`; lanes.md LANES-2 (the contract), LANES-4 (a step kind's default lane), LANES-7.

The cuts remove crash recovery and put positions in its place, so the surgery is not done until a kill test shows a reader resumes from its position and loses nothing committed.

## 2. Toys

Predictions are written and committed here before any code, as with every toy.

**T1, flows as events.** Westwind, on rannoch after the surgery, outside the rannoch crates. Three flows: a chain (host, then emit), a `count` by key, and a `wait` for a matching event; each with a row on the fast lane and a row on a slow lane. Random kills. Checked against an oracle in SQL over the same seed. To measure:
- The graph after kills equals the oracle.
- What a replay does. Today rannoch does not dedupe by CE id: the log mints a fresh id per event and the queue accepts a body it has accepted before; only a repeated `rannoch.node` or `rannoch.link` under the same id leaves the graph unchanged (docs/flows.md). So a replayed `flow.*` event is appended again and its next step runs again. T1 counts the repeated steps and checks that the graph is still the oracle's.
- Whether finding a parked `wait` by key seeks or scans, read from `EXPLAIN QUERY PLAN`: `_events_type` indexes (type, seq), not the subject.
- A poison event lands in the bin once, and the position moves past it.
- Facts a second, against today's fill path and the two-lane toy's 33,000.
- Log rows and bytes per fact, and what finished flows leave in `_events`.
- The status's cost: facts a second with one `_event_status` row appended per event against positions only; whether a reader's next event and the bin are found by a seek, from `EXPLAIN QUERY PLAN`; and how long a reader takes to find its lowest unprocessed event at start.

**T2, a direct Rust reader.** The same Westwind work as T1's chain, written as Rust on a lane with no flow and its own parsing. To measure: facts a second and log rows per fact against T1, so the cost of declaring a step as data is known.

## Open, for the operator

1. What happens to a finished flow's events in a log that keeps everything: kept, purged by the app's rule, or written to a memory lane that never reaches `_events`. T1's log measurement informs it.
2. Every step durable, or only a change of lane or a wait.
3. Retry defaults, operator 2026-11-09: "10, 20m? yes": 10 attempts, 20 minutes, skip-ahead on. Open: whether 20m is the wait between tries (about 3 hours for 10) or the whole window with exponential backoff inside it (legion's reading and preference).
4. When flow files are read: compiled into Rust at build (legion's proposal, to fit LANES-3), or kept as data in a watched folder, staged and made live by a signed request after a trial against history (the 2026-10-21 programs design; flows.md open question 6).
5. Whether rannoch dedupes by CE id at accept, so a replay writes nothing new. That is an addition, not a cut.
6. The surgery's scope (above).
