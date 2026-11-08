# Trash: spec draft

Draft for the operator's review, 2026-11-07. Nothing is filed or built from it. Every requirement cites what the operator said; what the operator has not decided is listed as open.

## Source

Operator, 2026-11-07:
- "add a trash, ejects to trash log, we will add a cleaner later. that will handle trims, purges is a very simple way"
- "I planned on reusing the trickle reader, if there are archive rules, we can use the trash log as a queue to archive or delete."
- "that also makes most of this reads and writes not deletes."
- "I thought this was a marked dead and move on?"
- "cause the trashing is an event"
- "everything is an event. everything, timeline, right?"
- "a purge would, anything marked dead, the question is do we disconnect the node at trash or purge." Answered: at trash.
- "un-trashing would just put the row back into the inbox, right?"

Background: rannoch's store is append-only, and its timeline is its events in order.

## Trash

1. **TRASH-1. Trashing is an event.** The trash processor (`rannoch-processor-trash`) SHALL send a cloud event of type `rannoch.trashed`, past tense like `rannoch.ended` and `rannoch.corrected` (operator: "whats wrong with trash?"), naming the fact and the reason. It goes through the inbox and onto the timeline like any other event.
2. **TRASH-2. Trash marks dead and moves on.** The trash event's write SHALL be one more version of the fact, saying dead, known at the trash event. It SHALL NOT change the timeline: as known before the trash, the fact reads as it was; as known after, dead.
3. **TRASH-3. Trash disconnects the node.** The trash event SHALL end each of the node's links, one more link version each, known at the trash event, so the live graph never reaches a dead node. As known before the trash, the links read as they were.
4. **TRASH-4. Nothing is deleted or updated by trash.** Every write trash makes is an append.
5. **TRASH-5. The trash log is the timeline's trash events.** The trash log is no separate stream: it is the trash events on the timeline, read by type.
6. **TRASH-6. Un-trash is the original event, sent again.** Sending the trashed fact's original event back through the inbox SHALL write the fact live again and reopen its links, as known at the re-send. It works until the fact is purged.
7. **TRASH-7. Trash is a processor.** It meets the processor contract (rannoch-processor), so the harness runs it like any other step.

## Later: the cleaner (not in this spec's build)

- The cleaner reuses the trickle reader, following the trash events on the timeline as its queue.
- The rules for what is archived or purged, and when, are the app's code, not rannoch's (operator: "we don't make up the guidelines for the rules. thats the app code. not rannoch"). Rannoch acts on an archive or purge event when the app sends one; archiving and purging are events on the timeline too.
- The cleaner handles purges. Rannoch offers no trim: a fact's history is kept for as long as the fact is (operator: "no trim"). A purge removes the content of anything marked dead from every copy: the store and the log. Rannoch keeps no hearings and no aliases (operator: "why do we need hearings, again?", "when do ever need a alias?"), and the inbox holds events in memory until a heartbeat commits them to the log, so it keeps no copy (rannoch-queue: "held in memory; a heartbeat commits everything accepted since the last one in one transaction"); the purge event stays on the timeline where it was. Finding and rewriting those copies is slow work, so a purge runs in the slow lane: a purge event's type routes it, in the build configuration, to `rannoch-queue-trickle`, which works off the log (operator: "the trickle since it works off the log is the path we take on a purge"), and the main path never waits on it (operator: "we need a slow lane again").

## Answered

- **Trash is delete** (operator, 2026-11-08: "Trash is the same as deleted so if you toss something in the trash, it show up? Ever?"): after a trash the node never shows up. Trash ends every link of the node, inbound and outbound, and the current read returns nothing for it. Only a read as of a point before the trash sees it as it was then, until a purge.
