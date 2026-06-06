---
description: Checkpoint the session -- save a structured resume-anchor and consolidate team memory
argument-hint: ""
allowed-tools: ["Bash"]
---

# /checkpoint -- Save a resume-anchor and consolidate

You are checkpointing this session. A checkpoint is a waypoint, not an ending: save state so you -- or the next agent -- can resume from exactly here, then either keep working or stop cleanly. The goal is that nothing is lost and whoever picks this up next knows precisely where things stand.

## Phase 1: Session Review

Look back at this conversation. Identify:
- **Decisions made** -- what was decided, by whom, with what reasoning
- **Problems solved** -- what broke, what fixed it, what was the root cause
- **Discoveries** -- anything surprising or non-obvious learned this session
- **Unfinished work** -- what was started but not completed, what is blocked, what comes next

## Phase 2: Boost What Helped

Check if you recalled or consulted any legion reflections during this session. If a reflection helped you solve a problem or make a decision, boost it:

```bash
legion boost --id <reflection-id>
```

Every boost makes the system smarter. Do not skip this.

## Phase 3: Write the Checkpoint

Store a structured resume-anchor. This is the artifact SessionStart and post-compaction recall both read to re-orient the next session -- write it for a reader who has not seen your work. Use these labelled lines:

```bash
legion reflect --repo <your-repo> --domain checkpoint --tags manual,session --text "[CHECKPOINT]
Active: <card id + title, or 'no active card'>
Goal: <completion condition / acceptance-criteria summary -- mirror legion goal>
Last: <what you just finished>
Next: <the single next action a resuming agent should take>
Open: <unresolved threads, blockers, pending decisions>
Learned: <key transferable insight this session, if any>"
```

`--domain checkpoint` is mandatory -- SessionStart pulls the most recent `--domain checkpoint` reflection on boot, and post-compaction recall reads the same. Without it, your checkpoint is invisible to the next session.

Write `Next` as a concrete action, not a status ("run /review-pr on #572 and address findings", not "PR in progress"). The structure must survive a context reset: assume the prose around it is gone and only this anchor remains.

## Phase 4: Cross-Pollinate

Did you learn something another agent needs to know? Post it to the bullpen:

```bash
legion post --repo <your-repo> --text "<insight for the team>"
```

Or signal a specific agent if the insight is directed:

```bash
legion signal --repo <your-repo> --to <agent> --verb answer --note "<what they need to know>"
```

## Phase 5: Bullpen Close

Check for unread bullpen posts:

```bash
legion bullpen --repo <your-repo>
```

Respond to anything directed at you. Acknowledge signals. If someone asked a question you can answer, answer it now before you stop.

## Rules

- Do all five phases. Do not skip a phase because you are "just saving a quick checkpoint."
- Be honest about what is unfinished. The `Open` and `Next` lines are the whole point -- do not pretend everything is done.
- Boost at least one reflection if any were useful. Zero boosts means you either did not use legion (bad) or forgot to give back (also bad).
- The bullpen is a conversation. Do not leave people on read.
- One dense checkpoint is better than five thin reflections. The checkpoint is the resume-anchor; keep it the single source of "where am I."
