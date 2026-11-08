# The core store: spec draft

Draft for the operator's review, 2026-11-07. Nothing is filed or built from it. It starts from rannoch v0.7.0-pre (44f07b4).

## Source

Operator, 2026-11-07: "store should be just the core store, everything else is optional and a processor." Also: "We do not delete this is a fucking temporal store"; "why do we need hearings, again?"; "when do ever need a alias?"; "no trim"; "cause the trashing is an event".

## The core store keeps

1. **STORE-1.** One write per fact: a changed fact appends a version; an unchanged fact writes nothing.
2. **STORE-2.** Append-only: no version or link version is ever deleted or updated.
3. **STORE-3.** Every class keeps history on its declared axis (an integer, a time, or arrival order) with known-at from the event's clock; there is no class without history (operator: "it's a bitemrporal store, none is silly"). The current row: the version with the highest axis value; between different facts at the same axis point, the latest known-at. Each version records the event that made it.
4. **STORE-4.** Links, and a link's changes (ended, corrected) as more link versions.
5. **STORE-5.** The trash event's write: a version saying dead, known at the trash event, and the node's links ended (spec-trash TRASH-2, TRASH-3).
6. **STORE-6.** Reads: the current row, as of an axis point, and as known at a time, answered from the versions.

## Out of the core store

7. **STORE-7.** Hearings and aliases are removed (no reader builds them either).
8. **STORE-8.** `seen` is removed. If something needs it later, a slow-lane reader can derive it from the log.
9. **STORE-9.** Amend moves to `rannoch-processor-amend`, a processor, built with the harness, not before it. Changing a fact's key is a rekey, not an amend (operator: "so a rekey?").
10. **STORE-10.** No trim.

## Crates

11. **STORE-11.** `rannoch-queue` becomes the lane contract both lanes meet. Today's in-memory queue moves to `rannoch-queue-mem`, behaviour unchanged, as the fast lane; the slow lane, following the log, is `rannoch-queue-trickle` (operator: "and a rannoch-queue-trickle ?").

## Existing stores

12. **STORE-12.** Stores written before this change move to the new layout by a migratr migration, not by SQL inside rannoch.

## Open, for the operator

1. Whether a rekey is its own event in the core store (it ends the old key, writes the new one, and keeps the old-to-new mapping every later write checks), with amend changing properties only.

2. Whether STORE-11 (the crate split) lands before the store changes, as a change with no behaviour difference.
