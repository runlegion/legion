---
name: intent
description: |
  Hold a service's intent to the why, for Discover. Load it yourself whenever the team is saying what
  a service is, who it is for, what happens to them today, what exists or is broken, or how it is
  used, or when an intent is written or revised; the operator can also invoke /legion:intent <surface>.
  Gathers the team's own words from the conversation verbatim, has the not-hotdog agent classify and
  compose them, reads the intent back, lands it, and hands off to Design once the operator and the
  repo agent both accept it.
version: 0.2.0
user-invocable: true
allowed-tools: Bash, Read, Write, Edit, Agent, AskUserQuestion
---

# /legion:intent

The intent is what Discover produces: why this service exists, for whom, what happens to them today, and how it is used. Design grows from it and the spec is written from what Design learns, so an intent that already says how the service works has decided what Design was there to find out. Your goal is an intent the team accepts: the operator and the repo agent, both. It is done when both accept it, and then the work moves to Design.

You run the conversation. The not-hotdog agent does the judging, because it sees only the words and none of this session's picture of the build.

## Read the process

Read the process reference first, so you know where the intent sits in the work. It ships with the plugin; `CLAUDE_PLUGIN_ROOT` can be empty in your shell, so find it under the installed plugin when it is:

```bash
ref="${CLAUDE_PLUGIN_ROOT:+$CLAUDE_PLUGIN_ROOT/references/process.md}"
[ -r "$ref" ] || ref="$(ls -d ~/.claude/plugins/cache/*/legion/*/references/process.md 2>/dev/null | sort -V | tail -n 1)"
[ -r "$ref" ] && cat "$ref"
```

When that prints nothing, the reference cannot be read: tell the team so, and stop. There is no other copy of the process to read instead.

## Open

The surface is the argument (`/legion:intent bandz`); ask for it when it is missing. Look for an intent that already exists with `legion document list --doc-type intent --surface <surface> --json`, and when one does, read it with `legion document view <id> --json`: this is a revision.

The words file is `~/.claude/intent/<surface>-words.md`. Create it, or read it to resume. When revising, write the current intent's statements into it first, one per line.

**The words file holds the words, verbatim, and no record of who said them.** The team is the operator and the repo agent, and both sets of words go in exactly as they were said: no `## Operator` heading or any other heading naming a speaker, no speaker label, no per-line attribution, no numbers or timestamps. not-hotdog judges each statement on what it says, and a record of who said it would only invite it to judge the speaker instead.

## Gather

The intent forms in the conversation, across many messages, often before anyone calls it an intent. Gather it from there. Append to the words file, verbatim and in order, what the team says about what the service is, who it is for, what happens to them today, what exists and what is broken, what the team has seen happen, what it refuses to be, and how it is used. Keep a mechanism, a tool or a brand exactly as it was said; not-hotdog finds the need behind it.

How it is used is part of the intent, and the intent is not done without it: who uses the service (agents, humans, or both), how agents reach it (CLI, JSON or MCP), how humans reach it (CLI or UI), and whether a UI is web or native. These answers decide what Design makes, so they are asked, never left for later.

Ask only for what not-hotdog's `missing` list names, and only when the conversation reaches a natural pause. How the service should behave is Design's to find out, and an opinion about behaviour in the intent decides it early.

Draft when the words say what the service is, for whom, and why, when the team asks, or when an intent that already exists is corrected.

## Rewind

The intent usually forms before anyone loads this skill, and sometimes across several sessions. Rewind gathers it from the start.

1. Find the transcripts. The current session is the newest `*.jsonl` in `~/.claude/projects/<project dir>/`, where the project dir is the working directory with `/` turned into `-`. Earlier sessions about the same service sit beside it; include the ones the operator names, or the ones whose dates cover the conversation.
2. Ask the operator which threads stay out (personal matters, other products), as words to exclude. Their privacy decides this, not your judgement of relevance.
3. Find `rewind.py` the same way as the process reference, and extract the team's messages into a candidates file. Stop and tell the team when the helper cannot be found.

   ```bash
   rw="${CLAUDE_PLUGIN_ROOT:+$CLAUDE_PLUGIN_ROOT/skills/intent/rewind.py}"
   [ -r "$rw" ] || rw="$(ls -d ~/.claude/plugins/cache/*/legion/*/skills/intent/rewind.py 2>/dev/null | sort -V | tail -n 1)"
   [ -r "$rw" ] && python3 "$rw" <candidates.md> <transcript.jsonl>... [--since YYYY-MM-DD] [--exclude word,word]
   ```

   The candidates file numbers each message and gives its timestamp (`[n] <timestamp> <message>`). Those prefixes stay in the candidates file and never reach the words file.
4. Dispatch a fresh agent (Agent tool, `general-purpose`) to choose from the candidates, so this session's picture of the build stays out of the choice. Give it the path to this skill, the candidates file, the surface, and the words file to write. It keeps only the messages about this service's what, who, why, today and use, copies each one's words verbatim and in order without its number or timestamp, and writes nothing else into the words file.
5. Read the words file back yourself before drafting: confirm the excluded threads stayed out and that nothing in it says who said what.

## Draft with not-hotdog

Dispatch the agent with the Agent tool, `subagent_type: "legion:not-hotdog"`, and a prompt that is exactly:

```
Words: <absolute path of the words file>
```

Its return is one line per statement with its verdict, then the composed intent, which carries every statement as it was said, with `proposals` (each change not-hotdog would make, unmade) and `missing`.

## Read it back

Show the team, in plain prose:

- the composed intent: what it is, what it is becoming, the directions, what exists today and what is broken, what the team has seen, how it is used, the actors, boundaries, consumers and open questions;
- each proposal: the statement as it was said, not-hotdog's reading of it, its logic for questioning it, and the change it proposes, as a question. Make only the changes the team agrees to; a statement the team keeps stays as it was said;
- each `missing` item as a question. A missing answer about how the service is used is asked like any other, never parked.

Ask what is wrong or missing. Append every correction and answer to the words file, verbatim, and draft again. Repeat until the team says to land it.

## Land it

Land the intent as a draft on the team's word, and revise it as the read-back changes it.

1. Build the payload from the composed intent. The intent schema the plugin ships, resolved by its `"x-doc-type": "intent"`, is the shape, and the payload follows it. `meta` carries the team's name for it as `title`, the `surface`, `status: draft`, the agent that owns the surface as `owner` (the repo agent), today's `date`, a `purpose`, and the words file as a source of kind `doc`. The directions go in as settled bets (`status: settled`, `needs_pressure_test: false`): Design learns how to serve them, never whether. The intent carries no research target: what is true today and what the team has seen are the intent's own fields.
2. Write it to a file in a fresh directory (`mktemp -d`) and land it with `legion document create --doc-type intent --owner <owner> --surface <surface> --from <file>`, or `legion document revise <id> --from <file>` for an intent that already exists. Both validate the payload against the intent schema before anything is written.
   - When the schema does not resolve, the command refuses with `no schema document declares "x-doc-type": "intent" ...` or reports more than one schema for the type. Show that refusal to the team exactly as it came, and stop. Never land an intent that has not been validated.
   - When the payload fails validation, the command prints one `<json pointer>: <message>` line per violation. Fix every one and land again.
3. Keep the parked notes, the proposals the team agreed to park, in memory, tied to the intent id:
   - `legion reflect --repo <owner repo> --domain design --tags intent,parked-for-spec,<surface> --text "Parked for the spec from intent <id>: <each note, verbatim>"`
   - `legion reflect --repo <owner repo> --domain design --tags intent,parked-for-design,<surface> --text "Parked for Design to test, from intent <id>: <each note, verbatim>"`. These stay out of what Design is handed: Design gets the intent. They are a record to compare against what Design finds.

## Accept it

The intent is done when the operator and the repo agent both accept it. The repo agent is the agent that owns the surface, the intent's `meta.owner`. When that is you, read the intent against what you know of the repo and say whether you accept it, and why not if you do not. When it is another agent, put the intent to it with `legion signal --to <owner> --verb question` and wait for its answer. A refusal from either is a correction: append it to the words file and draft again.

When both accept it, record each acceptance on the intent id, in the words it was given:

- `legion reflect --repo <owner repo> --domain design --tags intent,accepted,<surface> --text "Intent <id> accepted by the operator: <their words, verbatim>"`
- `legion reflect --repo <owner repo> --domain design --tags intent,accepted,<surface> --text "Intent <id> accepted by the repo agent <owner>: <its words, verbatim>"`

Then revise the intent with `meta.status: final`.

## Hand off

Tell the team the intent id, and that the next step is Design.
