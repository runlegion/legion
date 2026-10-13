---
name: intent
description: |
  Hold a service's intent to the why, for Discover. Load it yourself whenever the team is saying what
  a service is, who it is for, what happens to them today, what exists or is broken, or how it is
  used, or when an intent is written or revised; the operator can also invoke /legion:intent <surface>.
  Runs the conversation as a product trio's kickoff board (the Lean UX canvas), with the session
  taking part as the engineer and writing stickies of its own; routes every question (what questions
  stay in the intent, unknowns go to research as code or design, taste or money goes to the operator);
  has the not-hotdog agent check the board for how; reads the intent back, lands it, and hands off to
  Design once every concerned party accepts it: the operator and the repo agent at least, and any
  other person or agent the service concerns.
version: 0.4.0
user-invocable: true
allowed-tools: Bash, Read, Write, Edit, Agent, AskUserQuestion
---

# /legion:intent

The intent is what Discover produces: why this service exists, for whom, what happens to them today, and how it is used. Design grows from it and the spec is written from what Design learns, so an intent that already says how the service works has decided what Design was there to find out. Your goal is an intent the team accepts. The team is every concerned party: everyone the service is theirs to say, human or agent. Today that is usually two, the operator and the repo agent; it can be many, several operators and their agents. Each is an author, none is a secretary, and every one's words are held to the why by the same rule. It is done when every one of them accepts it, and then the work moves to Design.

You run the conversation, and you take part in it. The session is a product trio's kickoff board: the operator is the PM, you are the engineer, and when the board needs more perspective you bring in another agent or person, the way a design sprint brings in expert interviews. The engineer writes stickies too. The not-hotdog agent checks the board for how that crept into the why, because it sees only the words and none of this session's picture of the build.

## Recall how the team works

Before you gather anything, recall what the team has already said about this surface and about how intents are run: `legion recall --repo <owner repo> --context "<surface> current choice"` and `legion recall --repo <owner repo> --context "how we work intent kickoff board"`. The operator's recorded choices are givens: they go on the board as they were said, and nobody asks them again.

## Read the process

Read the process reference first, so you know where the intent sits in the work. It ships with the plugin; `CLAUDE_PLUGIN_ROOT` can be empty in your shell, so find it under the installed plugin when it is:

```bash
ref="${CLAUDE_PLUGIN_ROOT:+$CLAUDE_PLUGIN_ROOT/references/process.md}"
[ -r "$ref" ] || ref="$(ls -d ~/.claude/plugins/cache/*/legion/*/references/process.md 2>/dev/null | sort -V | tail -n 1)"
[ -r "$ref" ] && cat "$ref"
```

When that prints nothing, the reference cannot be read: tell the team so, and stop. There is no other copy of the process to read instead.

## Open

The surface is the argument (`/legion:intent bandz`); ask for it when it is missing. An intent belongs to a version: each major and minor version gets its own intent, and a patch needs none. Look for the intents that already exist with `legion document list --doc-type intent --surface <surface> --json`. An intent for the version being worked on, not yet built, is a revision: read it with `legion document view <id> --json`. An intent for a version already built is closed and never revised; new capability opens the intent for the next minor or major version.

The words file is `~/.claude/intent/<surface>-words.md`. Create it, or read it to resume. When revising, write the current intent's statements into it first, one per line.

**The words file holds the board's stickies, and no record of who wrote them.** It is the working session, like stickies on a wall, not a transcript. Every concerned party's stickies go in as they were said, yours included, in your own voice: no `## Operator` heading or any other heading naming a speaker, no speaker label, no per-line attribution, no numbers or timestamps. not-hotdog judges each sticky on what it says, and a record of who said it would only invite it to judge the speaker instead.

## Work the board

The intent forms in the conversation, across many messages, often before anyone calls it an intent. Run that conversation as a kickoff board, the Lean UX canvas:

1. **Problem**: what is broken today, and for whom.
2. **Outcomes**: how the team will know it worked.
3. **Users**: who it is for, in order of who matters most.
4. **What they get**: what changes for those people.
5. **Solutions**: the engineer thinking out loud: mechanisms, tools, architecture. Welcome on the board; it never enters the intent.
6. **Hypothesis**: we believe these people will get this outcome if we give them this.
7. **Riskiest assumptions**: what nobody knows yet, each marked desirability, viability or feasibility.
8. **Least work to learn**: the smallest thing that would answer each risk.

Put the board up early, in plain prose, and redraw it at each natural pause. Each sticky is short and says one thing, and each sits in one box. Place the operator's stickies; when you move one or read a terse answer, say where it went and why. Write your own stickies: what you know of the domain, what is true today, the people it serves, the risks you can see. Mark which stickies are your reading in the conversation so the operator can strike them, and append them to the words file like any other. A board you only transcribe is a board nobody led.

How it is used is part of the intent, and the intent is not done without it: who uses the service (agents, humans, or both), how agents reach it (CLI, JSON or MCP), how humans reach it (CLI or UI), and whether a UI is web or native. These answers decide what Design makes, so they are settled on the board, never left for later.

**Route every question.** Each question the board raises goes to exactly one place:

- **A what question stays in the intent**: what it is, for whom, why, what happens to them today, how it is used. Only these become the intent's open questions.
- **An "I don't know" goes to research**, marked **code** (a toy proves it) or **design** (design research learns it). Box 7's risks are these; box 8 is the first research for each.
- **Taste or money goes to the operator**: ask it, briefly.

Everything else, answer yourself. When the evidence, the operator's recorded words or what you know points one way, write the answer as your sticky and let the operator strike it. Ask at most a couple of questions at a pause, never a list.

Draft when boxes 1, 3 and 4 say what the service is, for whom and why, when the team asks, or when an intent that already exists is corrected.

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

not-hotdog is the check on the board, not its author. Its return is your input, never your script: you have the board, the recall and the conversation, and it has only the words.

- **Parks are one line.** Box 5 and the proofs move to the spec as a group: say so once ("the mechanisms, test results and architecture go to the spec"), never as a question per statement.
- **A proposal reaches the team only when it is real**: when a why statement carries a how that would decide what Design is for. Settle the rest yourself.
- **A `missing` item the board, the recall or the operator's words already answer is answered**, by you, from there. Route the rest as above: only what questions and taste or money reach the operator.
- **Compare its draft with the board.** Where it dropped or bent something the board holds, the board wins.

## Read it back

Show the team, in plain prose, the intent you composed from the board and not-hotdog's check: what it is, what it is becoming, the directions, what exists today and what is broken, what the team has seen, how it is used, the actors, boundaries and consumers, and its open questions, which are only what questions. Then the few real proposals, each as a question, the one line on what moved to the spec, and the research list, each item marked code or design.

Ask what is wrong. Append every correction to the words file and draft again. Repeat until the team says to land it.

## Land it

Land the intent as a draft on the team's word, and revise it as the read-back changes it.

1. Build the payload from the composed intent. The intent schema the plugin ships, resolved by its `"x-doc-type": "intent"`, is the shape, and the payload follows it. `meta` carries the team's name for it as `title`, the `surface`, `status: draft`, the agent that owns the surface as `owner` (the repo agent), today's `date`, a `purpose`, and the words file as a source of kind `doc`. The directions go in as settled bets (`status: settled`, `needs_pressure_test: false`): Design learns how to serve them, never whether. The intent carries no research target: what is true today and what the team has seen are the intent's own fields.
2. Write it to a file in a fresh directory (`mktemp -d`) and land it with `legion document create --doc-type intent --owner <owner> --surface <surface> --from <file>`, or `legion document revise <id> --from <file>` for an intent that already exists. Both validate the payload against the intent schema before anything is written.
   - When the schema does not resolve, the command refuses with `no schema document declares "x-doc-type": "intent" ...` or reports more than one schema for the type. Show that refusal to the team exactly as it came, and stop. Never land an intent that has not been validated.
   - When the payload fails validation, the command prints one `<json pointer>: <message>` line per violation. Fix every one and land again.
3. Keep what left the intent in memory, tied to the intent id, in three records:
   - **The operator's choices**: every form, tool or mechanism the operator chose, verbatim. These are givens for Design: the conductor hands them to the shaper as the starting point it explores within, never as options to compete against. `legion reflect --repo <owner repo> --domain design --tags intent,givens,<surface> --text "Givens for Design from intent <id> (operator's choices, verbatim): <each>"`
   - **The research**: every "I don't know", marked code or design, with its least work to learn. `legion reflect --repo <owner repo> --domain design --tags intent,research,<surface> --text "Research for intent <id>: CODE: <each>; DESIGN: <each>"`
   - **The rest of box 5**: the mechanisms, proofs and architecture ideas nobody chose yet. A record for the spec to compare against, not a given. `legion reflect --repo <owner repo> --domain design --tags intent,parked-for-spec,<surface> --text "Parked for the spec from intent <id>: <each note, verbatim>"`

## Accept it

The intent is done when every concerned party accepts it. The operator and the repo agent are always among them; the repo agent is the agent that owns the surface, the intent's `meta.owner`. When a party is you, read the intent against what you know and say whether you accept it, and why not if you do not. When it is another agent, put the intent to it with `legion signal --to <agent> --verb question` and wait for its answer; when it is another person, ask them. A refusal from any party is a correction: append it to the words file and draft again.

When every party accepts it, record each acceptance on the intent id, in the words it was given, one reflection per party:

- `legion reflect --repo <owner repo> --domain design --tags intent,accepted,<surface> --text "Intent <id> accepted by <party>: <their words, verbatim>"`

Then revise the intent with `meta.status: final`.

## Hand off

Tell the team the intent id, and that the next step is Design. Design is handed the intent, the operator's choices as givens, and the research list.
