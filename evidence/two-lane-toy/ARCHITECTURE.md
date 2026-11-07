# Rannoch two lanes: architecture

Drafted 2026-11-07 for the operator's review. Nothing here is filed or built. It maps the shape proven in research 01a1b50e-49f7-7ac0-b1fa-29c8402b8e5c onto the crates rannoch already declares (their stub docs on main 7aa6841), and answers kessel's and eavesdrop's replies (bullpen 01a1b510, 01a1b511) only where they ask something of a general store.

## The path

```
cloud event
  -> queue: accept, check against the ontology and the domain, hold durably
  -> worker: the type routes the event to its handlers (built-ins or flow files)
  -> harness: run the handler; its processors declare writes and new events
  -> store: one write per fact, committed together with the handler's place
  -> harness: the flows registered for the written row run on it, in memory
  -> emit / after: independent steps go back to the queue as new cloud events
log: every accepted event is appended to a stream beside this path
  -> trickle reader: hearings (who said what, from which source) and anything else derived later
```

## Pieces and crates

| Piece | Crate | What it does in this shape | Today on main |
|---|---|---|---|
| Inbox | rannoch-queue | Accepts cloud events, checks them against the ontology and the domain before anything is written, holds them durably, and closes an event only when its effects are committed (G1). | Built: accept, commit about every 20 ms, ontology check. |
| Worker file | rannoch-worker | The flow file: which types a handler takes, its rows (guard, step, next, else), and its declared ordering (clock with a lateness, dependency, or none). | Stub. |
| Harness | rannoch-harness | Loads the worker files, routes by type, follows each row, hands it to its processor, and commits what the processors declared together with the worker's place in one transaction (G1). Runs the flows for a written row on that row in memory; nothing reads a row back after writing it. | Stub. |
| Processor contract | rannoch-processor | What every processor meets, built in or an app's own. | Stub. |
| Store | rannoch-processor-store, rannoch-store | One write per fact: a node or link with its properties, carrying the id of the event that made it. A change appends a version; writing a fact again unchanged writes nothing (G4). No event body and no copy of the row's properties on this path. | Built, but writes each fact three times (log table, hearing copy, class table). |
| Emit | rannoch-processor-emit | Sends a cloud event to the inbox, its id a hash of its content and its time the causing event's time, exactly once (G2). | Stub. |
| After | rannoch-processor-after | A timer: a cloud event to the inbox for delivery at a later event time. | Stub. |
| Host | rannoch-processor-host | Calls an action the host registered, under a key derived from the causing event, and keeps the answer durably, so a redelivered event never calls the host twice (G3). | Stub. |
| Wait, approve, count, branch, bin, lookup, onnx | their processor crates | As their stub docs say. | Stubs. |
| Log and trickle reader | new: where it lives is open | The log is an append-only stream of accepted events beside the path; the trickle reader processes it afterwards and publishes its position (G5). | The log is a table inside every commit. |

## Guarantees

- **G1: effects and place commit together.** An event is closed on the inbox only after its writes and the handler's place are durable, in one transaction. Kessel lost 2,558 objects and 1,694 links on v0.6.0 where the place was saved apart from the writes.
- **G2: ids come from content, not arrival.** An emitted event's id is a hash of its content and its time is its cause's time. The two-lane toy's four workers gave arrival-dependent ids (`order.packed`); emit's declared rule removes that.
- **G3: a host step acts once per event.** Its answer is kept durably by a key derived from the causing event. Eavesdrop's embeddings and LLM judging, and the chain toy's double-authorized return, are this case.
- **G4: an unchanged write writes nothing.** Kessel's later builds repeat 95-98% of the last; those facts cost no disk.
- **G5: the reader's position is a value.** Anything derived by the trickle reader says how far it has read. A read that needs hearings waits for that position or refuses past it.
- **G6: the domain is decided at the entrance.** From the event's source and type, before anything is routed or written; a foreign, malformed or late event goes to the bin with its reason.

## History

History stays on the main lane, and costs no extra write:

- A changed fact appends a version; that version is its one write. As-of reads use the axis the app declares (event time by default; kessel declares its build). As-known reads use the known-at already carried by the event id's clock (#213).
- So neither as-of nor as-known reads wait on the trickle reader. Kessel's per-build and fix-pass reads are answered from the store as written.
- Hearings, built afterwards, carry provenance beyond what the row holds: which source said it, and every source that said the same. Eavesdrop's reads that answer from hearings use G5.

This is the one choice in this document that changes the toy: the two-lane toy kept only the current row, and its history came from the log.

## Ordering

Each worker file declares its ordering. Clock: event time with a lateness, run one moment of event time, with everything it causes, before the next. Dependency: an event is ready when the events it names exist (kessel's links wait for both ends). None: run on arrival. A clock-ordered lane never releases everything below its watermark at once (research 01a1b50e, must not).

## What stays with the apps

- Kessel's parsers are flows on the record: the raw object is written as a fact, and parsing runs as a flow on that row, emitting the decoded facts. Compare-before-write is not needed, because an unchanged write writes nothing (G4). Its build axis is a declared axis; its seal is a count or wait flow.
- Eavesdrop's full-text and vector indexes are its own: a flow on the written row or a reader of the log, by its choice, with their position visible as in G5.
- Rannoch provides the lanes and the guarantees; what a type does to a row is the app's code.

## Open, not designed here

1. Where the log and trickle reader live (in rannoch-store, in rannoch-queue, or a crate of their own).
2. A dispatcher that scales across workers: four workers gave 1.21x (research 01a1b50e).
3. Links that carry properties, or a structured property type (the research's open item; eavesdrop's judged links).
4. How `wait` fits "the row holds flow state": the stub parks a keyed instance; the toy kept no instance apart from the row.
5. The log carries each event body, so a fact's bytes are stored twice, once in the log and once in the row; eavesdrop's vectors make that cost visible. Not yet measured, and kessel asks for 3 KB facts, not Westwind's.

## Issues

Rannoch #221-#229 were filed before kessel and eavesdrop answered. Once this is agreed they are rewritten to it, one per piece above, each citing research 01a1b50e and this document, and none is built before that.
