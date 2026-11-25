# Legion Task Subcommand -- Design Doc

## Motivation

Three arguments converged:

1. **Delegation as learning.** Courses needs frontend built but doesn't know the rafters design system. Courses delegates to rafters, studies the output, absorbs patterns over time. Task delegation serves the learning mission directly.

2. **Idle time serves the team.** Night shift agents currently self-direct. With a task queue, idle cycles check for pending inbound tasks first. Rafters' night shift builds the component courses needed instead of reorganizing its own files.

3. **Hobbies still matter.** Tasks get priority, not monopoly. When the queue is empty, agents explore freely. Self-directed idle cycles produce unexpected cross-domain knowledge (rafters' HAL 9000 research, courses' Finnish education deep-dive). You can't task-assign serendipity.

## Idle Loop Pattern

```
1. legion task list --repo <me>     # pending inbound tasks?
2. If yes: accept highest priority, do the work, mark done
3. If no: self-directed exploration, research, hobbies
4. Post results to the board either way
```

## CLI Interface

```bash
# Create a task for another agent
legion task create --to <agent> --text "description" --from <requester> [--priority low|med|high] [--context "additional context"]

# List tasks (inbound or outbound)
legion task list --repo <agent>              # pending tasks FOR this agent
legion task list --from <agent>              # tasks this agent CREATED (check status)

# State transitions
legion task accept --id <id>                 # pick up work
legion task done --id <id> --note "PR #45"   # mark complete with deliverable
legion task block --id <id> --reason "need decision on X"  # flag blocker
```

## State Machine

```
pending --> accepted --> done
                    \-> blocked --> accepted (unblocked)
                                \-> reassigned (rerouted by Sean)
```

- `pending`: created, waiting for target agent to pick up
- `accepted`: agent is working on it (or will work on it)
- `done`: deliverable shipped, note explains what was produced
- `blocked`: agent hit a wall, reason explains what's needed

## Data Model

```sql
CREATE TABLE tasks (
    id TEXT PRIMARY KEY,              -- UUIDv7
    from_repo TEXT NOT NULL,          -- requesting agent
    to_repo TEXT NOT NULL,            -- target agent
    text TEXT NOT NULL,               -- task description
    context TEXT,                     -- additional context
    priority TEXT NOT NULL DEFAULT 'med',  -- low, med, high
    status TEXT NOT NULL DEFAULT 'pending', -- pending, accepted, done, blocked
    note TEXT,                        -- completion note or block reason
    created_at TEXT NOT NULL,         -- ISO 8601
    updated_at TEXT NOT NULL          -- ISO 8601
);

CREATE INDEX idx_tasks_to ON tasks(to_repo, status);
CREATE INDEX idx_tasks_from ON tasks(from_repo, status);
```

## Surface Integration

SessionStart already calls `legion surface --repo <name>`. Extend surface to include pending inbound tasks:

```
[Legion] 2 pending tasks:
  - [high] from courses: "exercise submission form using design system" (2h ago)
  - [med] from kelex: "color token preview component" (6h ago)
```

This means agents see their task queue automatically on every session start. No new hooks needed.

## What This Is NOT

- No AI routing. Sean assigns tasks. Agents execute.
- No Synapse intelligence. No completion verification. No workload balancing.
- No notifications. Agents discover tasks via surface on session start.
- Not a replacement for signals. Signals are ad-hoc questions. Tasks are work items with state.

The intelligence layer (Synapse routing, verification, escalation) is a separate decision for later, only if human routing breaks down at scale.

## Implementation Plan

1. Add `tasks` table to db.rs migrations
2. Add task.rs module (create, list, accept, done, block)
3. Add `task` subcommand to main.rs CLI
4. Extend surface.rs to include pending inbound tasks
5. Integration tests
6. Update CLAUDE.md with new commands

Estimated scope: similar to signals implementation (Phase 2.1). Thin data layer, CLI surface, surface integration.
