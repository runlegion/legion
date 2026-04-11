---
name: legion-memory
description: |
  Auto-triggered skill that reminds agents of the recall-before-grep doctrine. When an agent is about to search the codebase for answers, this skill reminds them to check legion memory first. Use this when the agent is researching a problem, looking for patterns, or trying to understand why something was built a certain way.
version: 1.0.0
user-invocable: false
allowed-tools: Bash, Read
---

# Legion Memory: Recall Before Grep

Before searching the codebase, check legion memory. Code tells you WHAT exists. Legion tells you WHY it exists, WHAT went wrong last time, and WHAT the person who solved it wished they had known.

## Order of Operations

1. `legion recall --repo <current-repo> --context "<your problem>"` -- search your own memory
2. `legion consult --context "<your problem>"` -- search ALL agents if recall did not help
3. THEN grep, glob, read the codebase

## When You Find Useful Reflections

Boost them: `legion boost --id <reflection-id>`

This makes them surface higher for the next agent. Every boost improves the system.

## When You Learn Something New

Reflect it: `legion reflect --repo <current-repo> --text "<what you learned>"`

Capture the WHY, not the WHAT. The code already shows what you did. The reflection should capture:
- Why you chose this approach over alternatives
- What you tried that did not work
- What surprised you
- What the next agent should know

## Cross-Agent Knowledge

Use `legion consult` when you hit something outside your domain. Another agent may have already solved it. The knowledge is indexed and ranked -- useful reflections rise, stale ones fade.

## Bullpen Awareness

Check the team board periodically: `legion bullpen --repo <current-repo>`
Respond to signals directed at you. Post findings that the team needs to see.
