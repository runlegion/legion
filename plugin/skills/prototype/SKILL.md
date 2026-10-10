---
name: prototype
description: |
  Make the interface prototypes for an intent during Design, on the Design canvas (Claude's /design),
  and record each one as a pointer to its canvas and the part of the interface it explores. Load it
  yourself whenever Design for an accepted intent needs to show an interface before anyone builds it,
  or when a prototype has been made and is not yet recorded; the operator can also invoke
  /legion:prototype <intent id>. The pointers are what the brief-writer draws the design brief from.
version: 0.1.0
user-invocable: true
allowed-tools: Bash, Read, Write, Artifact, AskUserQuestion
---

# /legion:prototype

A prototype shows an interface before anyone builds it, so the team can see it, argue with it, and change it while change is cheap. Your goal is that every interface the intent calls for has been tried on the Design canvas, and that the team and the brief-writer can find every one of those prototypes afterwards without being told where to look. A prototype that nobody can find is a prototype the design brief is not drawn from.

Prototypes run together with proofs and the documents, each informing the others. You make the prototypes; a proof or a change to the prose can send you back to make another, and a prototype can send the team back to the prose.

## Read the process

Read the process reference first, so you know where prototypes sit in the work. It ships with the plugin; `CLAUDE_PLUGIN_ROOT` can be empty in your shell, so find it under the installed plugin when it is:

```bash
ref="${CLAUDE_PLUGIN_ROOT:+$CLAUDE_PLUGIN_ROOT/references/process.md}"
[ -r "$ref" ] || ref="$(ls -d ~/.claude/plugins/cache/*/legion/*/references/process.md 2>/dev/null | sort -V | tail -n 1)"
[ -r "$ref" ] && cat "$ref"
```

When that prints nothing, the reference cannot be read: tell the team so, and stop.

## Who the interface is for

The intent decides whether there is anything to prototype. Read it with `legion document view <intent id> --json` and look at who uses the service, under `interface.users`.

A prototype here is a UX prototype: it exists to show a person what they will see and do. When the intent says only agents use the service, there is no person to show, so Design makes no prototype. Say so to the team, say that this is why, and stop.

When the intent does not say who uses the service, you do not know whether a prototype is wanted at all. Ask the team before you make anything, and wait for the answer; a guess here either wastes a canvas or skips the interface a person needed.

When people use the service, the intent's other interface answers tell you what to prototype: how people reach it, and whether the interface is a web or a native UI.

## Making a prototype

Each prototype is made on the Design canvas, Claude's /design, which is always available to legion: start it with the Artifact tool's `quickstart` and the `design` intent, then build the prototype there. The prototype lives on its canvas and nowhere else. It is not a file in the repo, and you never copy it into one.

Give each prototype one part of the interface to explore, and know which part before you start: a screen, a flow through several screens, one moment that is hard to get right. The part is how the brief-writer and the team will know what the canvas is for without opening it.

If the Design canvas cannot be reached in this session, tell the team so and stop. A pointer is only ever recorded for a canvas that was made, so there is nothing to record.

## Recording the pointer

As soon as a prototype exists, record it as one `prototype` document: a pointer to its canvas and the part it explores, naming the intent it was made for. Write the payload to a file in a fresh directory (`mktemp -d`):

```json
{
  "meta": { "intent": "<intent id>", "date": "<YYYY-MM-DD>", "author": "<your agent id>" },
  "canvas": "<the canvas link>",
  "explores": "<the part of the interface it explores>"
}
```

and land it:

```bash
legion document create --doc-type prototype --owner <your agent id> --from "$dir/prototype.json"
```

Create checks the payload against the prototype schema before anything is written. When it prints `<json pointer>: <message>` lines, fix each one and create again.

## Finding the prototypes

Every prototype made for an intent is found the same way, by the team, by the brief-writer, and by you when you resume:

```bash
legion document list --doc-type prototype --json \
  | jq --arg intent "<intent id>" \
      '[.[] | (.payload | fromjson) as $p | select($p.meta.intent == $intent)
        | {id, canvas: $p.canvas, explores: $p.explores}]'
```

The design brief for the interfaces is drawn from these prototypes, and the brief-writer finds the pointers this way itself.
