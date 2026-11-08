# Building an app on rannoch: what the app does

Draft for the operator's review, 2026-11-07. Rannoch is general software and makes no policy: it provides the inbox, the store, the timeline and the steps. This guide lists what each app does itself. Every entry cites the operator's words; nothing here is a rannoch feature.

## Decided

1. **What an event type does to a row.** The app's worker code decides it; rannoch cannot know. (Operator, 2026-11-07: "It can't know"; "Now is for the worker code to decide not rannoch".)
2. **What runs now and what runs after the write.** The worker code decides. (Same.)
3. **How a flow's events are ordered.** Each flow chooses: by clock, by dependency, or neither. (Operator: "It's both clock could be the order, it depends on the flow".)
4. **Parsing is a flow on the record.** An app that parses raw input, as kessel does, writes the raw record and parses it in a flow on that record, not before the write. (Operator: "Kessels parsers are workflows on the record not prior".)
5. **Archive and purge rules.** Which trashed facts are archived or purged, and when, is the app's code. Rannoch acts on an archive or purge event when the app sends one. (Operator: "we don't make up the guidelines for the rules. thats the app code. not rannoch".)
6. **No trim.** Rannoch keeps a fact's history for as long as the fact. An app that wants facts gone trashes them, and its own rules purge them. (Operator, 2026-11-07: "no trim".)

## How to keep this list

An entry is added here when the operator decides that something is the app's, with the operator's words cited. Nothing is added from a guess about what an app will need.
