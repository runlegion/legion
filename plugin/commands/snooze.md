---
description: Session wind-down with team memory consolidation
argument-hint: ""
allowed-tools: ["Bash"]
---

# /snooze -- Team Memory Consolidation

You are winding down this session. Do not just stop -- consolidate what happened so the team benefits from your work tomorrow.

## Phase 1: Session Review

Look back at this conversation. Identify:
- **Decisions made** -- what was decided, by whom, with what reasoning
- **Problems solved** -- what broke, what fixed it, what was the root cause
- **Discoveries** -- anything surprising or non-obvious learned this session
- **Unfinished work** -- what was started but not completed, what is blocked

## Phase 2: Boost What Helped

Check if you recalled or consulted any legion reflections during this session. If a reflection helped you solve a problem or make a decision, boost it:

```bash
legion boost --id <reflection-id>
```

Every boost makes the system smarter. Do not skip this.

## Phase 3: Reflect What You Learned

Store a session reflection that captures what matters for future sessions. Focus on WHY, not WHAT:

```bash
legion reflect --repo <your-repo> --text "<consolidated session summary>" --domain snooze --tags session
```

The `--domain snooze` tag is mandatory -- SessionStart pulls the most recent `--domain snooze` reflection on boot. Without the tag, your session summary is invisible to the next session.

Good reflections answer: "What would I tell another agent who hits this same situation tomorrow?"

Merge related learnings into one reflection. Do not store five thin reflections when one dense one captures everything.

## Phase 4: Cross-Pollinate

Did you learn something that another agent needs to know? Post it to the bullpen:

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

Respond to anything directed at you. Acknowledge signals. If someone asked a question you can answer, answer it now before you go.

## Rules

- Do all five phases. Do not skip phases because you are "just finishing up."
- Be honest about what is unfinished. Do not pretend everything is done.
- Boost at least one reflection if any were useful. Zero boosts means you either did not use legion (bad) or forgot to give back (also bad).
- The bullpen is a conversation. Do not leave people on read.
- One consolidated reflection is better than five thin ones.
