---
name: hedge-sweep
description: Rewrite decision-log hedges across the legion document store as the rule and its reason. A hedge is a decision written like a meeting minute -- "Current choice (2026-10-20, Sean: 'yes'): X; revisable", "Approved 2026-10-25 (#520); revisable". Run it when hedges turn up in requirements, research or intents, or after an instruction file is found seeding the pattern. Haiku 5.5 proposes a rewrite per paragraph; nothing is written until the operator has seen the review and you pass --write.
---

# Hedge sweep

Agents copy the wording of what they read, not only its rules. One hedged line in an instruction file became how the team recorded decisions: requirements, research and code comments filled with dates, names and "revisable", and every rule read as provisional. A builder needs the rule and why it holds, not who chose it or when.

Fixing the copies one at a time misses most of them. The sweep finds every hedged paragraph in the store, has Haiku 5.5 rewrite it, and splices the answer back into the document's JSON itself, so criterion ids and structure never pass through the model.

## Steps

Find `sweep.py` beside this file. `CLAUDE_PLUGIN_ROOT` can be empty in your shell, so fall back to the installed plugin:

```bash
sw="${CLAUDE_PLUGIN_ROOT:+$CLAUDE_PLUGIN_ROOT/skills/hedge-sweep/sweep.py}"
[ -r "$sw" ] || sw="$(ls -d ~/.claude/plugins/cache/*/legion/*/skills/hedge-sweep/sweep.py 2>/dev/null | sort -V | tail -n 1)"
```

Use a working directory in your scratchpad.

1. **Trial.** `python3 -I "$sw" propose <workdir> <id> <id> <id>` on three documents: two plain hedges and one that must be kept (a withdrawal, or a rule about revising). Read `python3 -I "$sw" review <workdir>`. Fix the prompt before going wider.
2. **Propose.** `python3 -I "$sw" propose <workdir>` over the whole store. About a tenth of a cent per paragraph.
3. **Review.** `python3 -I "$sw" review <workdir>` lists flagged proposals first. Most flags are provenance ids dropped on purpose; look for a real number, condition or reason that went missing, and fix it by editing `after` in `proposals.jsonl`. Show the operator the review and get a yes before writing: the store is every repo's, not only yours.
4. **Apply.** `python3 -I "$sw" apply <workdir>` writes each new payload under `payloads/` without touching the store. Then `apply <workdir> --write` revises each document. A document whose schema refuses the payload is reported and skipped; a rerun refuses paragraphs already changed, so it never writes twice.

## What it leaves alone

- Fields that hold history or provenance: `stickies`, `provenance`, `answers`, `answered`, `traces_to`, `from`.
- A withdrawal ("E4's current choice is withdrawn"), a rule about revising ("answers are current findings, revisable when new evidence arrives"), and text about the habit itself. The prompt keeps those words and still rewrites any other hedge in the same paragraph.
- A real condition for revisiting a choice ("once 0.5 logs real predictions") is content, not a hedge. Haiku sometimes drops one; the review step is where you catch it.
