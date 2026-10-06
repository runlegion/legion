---
name: intent
description: |
  Hold a service's intent to the why: step 1 of the double diamond. Load it yourself whenever the
  operator is saying what a service is, who it is for, what happens to them today, or why it matters,
  or when an intent is written or revised; the operator can also invoke /intent <surface>. Gathers the
  operator's own words from the conversation verbatim, has the not-hotdog agent classify and compose
  them, reads the intent back, and lands it with its parked notes on the operator's word.
version: 0.1.0
user-invocable: true
allowed-tools: Bash, Read, Write, Edit, Agent, AskUserQuestion
---

# /intent

**This is a design discovery workshop, and this is step 1, the intent.** Read the double diamond first: `legion document view --slug double-diamond --json`. Do this well, and then there will be code.

You run the conversation. The not-hotdog agent does the judging, because it sees only the operator's words and none of this session's picture of the build. Your own view of how the service should work stays out of the intent: the intent is the operator's why.

## 1. Open

- The surface is the argument (`/intent bands`). Ask for it when it is missing.
- Look for an existing intent: `legion document list --doc-type intent --surface <surface> --json`. When one exists, read it (`legion document view <id> --json`): this is a revision.
- The words file is `~/.claude/intent/<surface>-words.md`. Create it, or read it to resume. When revising, write the current intent's statements into it first, under `## Current intent`, one per line.

## 2. Gather

The intent forms in the conversation, across many messages, often before anyone calls it an intent. Gather it from there:

- Append the operator's own messages that say what the service is, who it is for, what happens to them, what it refuses to be, or how they picture it, to the words file under `## Operator`, verbatim. Their words only: what you said stays out, even when they agreed with it. When they adopt a phrase of yours, record their message that adopts it.
- Keep a mechanism, a tool or a brand in the words exactly as they said it; not-hotdog finds the need behind it.
- Ask only for what not-hotdog's `missing` list names, and only when the conversation reaches a natural pause. Leave how the service should behave to the workshop and the spec agents: an opinion about behaviour in the intent shades the workshop.

Draft when the words say what the service is, for whom, and why, when the operator asks, or when they correct an intent that already exists.

## 2b. Rewind

The intent usually forms before anyone loads this skill, and sometimes across several sessions. Rewind gathers it from the start.

1. Find the transcripts: the current session is the newest `*.jsonl` in `~/.claude/projects/<project dir>/` (the project dir is the working directory with `/` turned into `-`). Earlier sessions about the same service sit beside it; include the ones the operator names, or the ones whose dates cover the conversation.
2. Ask the operator which threads stay out (personal matters, other products), as words to exclude. Their privacy decides this, not your judgement of relevance.
3. Extract the operator's own messages: `python3 ~/.claude/skills/intent/rewind.py <candidates.md> <transcript.jsonl>... [--since YYYY-MM-DD] [--exclude word,word]`.
4. Dispatch a fresh agent (Agent tool, `general-purpose`) to do the Gather selection, so this session's picture of the build stays out of the choice: give it the path to this skill, the candidates file, the surface, and the words file to write. It keeps only messages about this service's what, who and why, verbatim, in order, and writes them under `## Operator`.
5. Read the words file back yourself before drafting: confirm the excluded threads stayed out.

A message whose meaning depends on something the agent said ("good catch, how do we do that?") arrives without its referent. not-hotdog lists it under `missing`; ask the operator what it referred to, and append the answer verbatim.

## 3. Draft with not-hotdog

Dispatch the agent with the Agent tool, `subagent_type: "not-hotdog"`, and a prompt that is exactly:

```
Words: ~/.claude/intent/<surface>-words.md
```

Read its return: one line per statement with its verdict, then the composed intent with `parked_for_workshop`, `parked_for_spec`, and `missing`.

## 4. Read it back

Show the operator, in plain prose:

- the composed intent: what it is, becoming, the directions, current state, actors, boundaries, consumers, open questions;
- what was parked for the spec agents, so they can see their how was kept;
- what was parked for the workshop, as needs the workshop should find rather than the intent state;
- each `missing` item as a question.

Ask what is wrong or missing. Append every correction and answer to the words file, verbatim, under `## Operator`, and draft again (step 3). Repeat until the operator accepts the intent as written.

## 5. Land it, on the operator's word

1. Resolve the intent schema: the row of `legion document list --doc-type schema --json` whose payload carries `"x-doc-type": "intent"` (the payload is a JSON string; parse it twice).
2. Build the payload from the composed intent:
   - `meta`: `title` (the operator's name for it), `surface`, `status: draft`, `owner` (the agent that owns the surface), `date` (today), `purpose` ("The statement the workshop grows from. Details belong to the workshop and the spec agents."), `sources` (the words file, kind `doc`).
   - `what_it_is`, `current_state`, `boundaries`, `actors`, `consumers`, `open_questions`: from the composed intent.
   - `direction.becoming`; `direction.proposals`: one per direction, `{"text": <direction>, "status": "settled", "needs_pressure_test": false}`. The directions are the operator's bets: the workshop learns how to serve them, never whether.
   - `evidence`: `{"lenses": [<eavesdrop lenses the operator names, or none>], "needs_crawl": true, "crawl_topic": <one line from what_it_is>}`.
3. Validate: `legion document validate --schema <schema-id> < <payload file>`, and fix every error.
4. Create or revise: `legion document create --doc-type intent --owner <owner> --surface <surface> --from <payload file>`, or `legion document revise <id> --from <payload file>`.
5. Keep the parked notes in memory, tied to the intent id:
   - `legion reflect --repo <owner repo> --domain design --tags intent,parked-for-spec,<surface> --text "Parked for the spec agents from intent <id>: <each note, verbatim>"`
   - `legion reflect --repo <owner repo> --domain design --tags intent,parked-for-workshop,<surface> --text "Needs parked for the workshop to find, from intent <id>: <each>"`. These stay out of the workshop's input: the workshop gets only the intent. They are a record to compare against what the workshop finds.

## 6. Hand off

Tell the operator the intent id and that the next step is the workshop (`/workshop` on that intent).
