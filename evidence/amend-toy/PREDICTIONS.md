# Amend toy: predictions

Written 2026-11-07, before anything is built. It extends the version toy (rannoch toys/version at b82372f) with rannoch's amends system as a type worker, so rannoch's amend, correct, end and alias behaviour is toy-backed before #221, #222 and #223 are written as one issue. It changes only what these predictions name.

## The amends system today (rannoch main 7aa6841, docs/store.md)

- `rannoch.amended`: its subject is the log id of the event it amends, and its data is a JSON Merge Patch of that event. It corrects what an event said without changing the event; a patch to the subject renames what the event was about.
- `rannoch.corrected.<relation>`: a link was wrong. `rannoch.ended.<relation>`: a link stopped holding.
- The same content arriving again under another id is an alias of the first hearing.

## What the toy builds

- An amend worker takes `rannoch.amended`. It finds the facts the amended event made from the hearings, waiting until the trickle reader's position has passed the amended event, and writes each fact's patched version at its original axis point, known at the amendment's id.
- A patch to the subject ends the old key's fact and writes the new key's, both known at the amendment.
- Corrected and ended links are each one more version of the link.
- An alias writes nothing to the store; the hearings keep every id that said the same content.
- Nothing on the main path reads the log or the hearings; only the amend worker reads the hearings.

## The data

The version toy's build stream (200,000 objects of about 3 KB, 6 builds, the fix pass after build 4) and the Westwind stream, with a seeded set added: 2,000 amendments of property values, 200 amendments that change a subject, 500 corrected links, 500 ended links, and 2,000 aliases (identical content under a new id), spread across the builds. No kessel data is used.

## Predictions

1. For every amended fact, an as-known read before the amendment gives the original and after it gives the patched value; as-of reads at the fact's axis point give the patched value as known now. 2,000 of 2,000.
2. For every subject amendment, the old key reads as ended and the new key as present, as known after the amendment; as known before, the old key is present and the new key absent. 200 of 200.
3. Corrected links read as wrong and ended links as no longer holding, each as known after its event and as before it known before. 500 of 500 each.
4. Aliases write nothing to the store, and the hearings list both ids for each. 2,000 of 2,000.
5. An amendment that arrives before the trickle reader has passed the event it amends waits, and is applied once the reader passes it; none is applied to the wrong facts.
6. The amend worker never slows the main path: the build stream's rate with amendments flowing is within 5% of the version toy's 21,700-21,900 objects/s.
7. 10 aborts at random rounds with amendments in flight give the same versions as an unkilled run, and no amendment is applied twice.

## What breaks the idea

- The amend worker cannot find an amended event's facts from the hearings.
- An amendment needs the main path to read the log or the hearings.
- A rename leaves the old and new keys both present, or both absent, as known after it.
