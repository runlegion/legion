# AI-First Kanban for Legion

**Date:** 2026-03-21
**Status:** RFC -- awaiting Sean's review
**Context:** Sean manages ~12 AI agents from the legion dashboard. The current task system is CLI-driven with a read-only kanban view. He needs a real control surface.

## Core Insight

Traditional kanban: humans manage their own boards. AI-first kanban: one human manages a fleet of agents. The board is the manager's control surface, not the workers'.

## What Makes This Different from GitHub Projects / Jira

1. **One board, all agents.** Not per-project boards. Sean sees everything across all agents.
2. **Agents as workers.** Sean assigns, agents execute. Agents can also delegate to each other.
3. **Idle visibility.** Knowing which agents have empty queues is actionable -- wasted capacity Sean can redirect.
4. **Delegation chains.** Task A spawns task B spawns task C. The board shows the chain.
5. **Signals as state.** A review request or blocker is a card state, not a comment.
6. **Global priority.** Sean prioritizes across the whole team, not per-agent backlogs.
7. **No ceremony.** No sprints, no story points, no velocity charts. AI agents don't have calendars.

## Column Layout (6 columns)

| Backlog | Pending | In Progress | Needs Input | In Review | Blocked |
|---------|---------|-------------|-------------|-----------|---------|

- **Backlog** -- Unassigned ideas. Sean's scratch pad. `to_repo` is `_backlog`. Agents can self-assign from here.
- **Pending** -- Assigned to an agent, not yet picked up.
- **In Progress** -- Agent is working (maps to `accepted` status internally).
- **Needs Input** -- Agent is stuck waiting on Sean. These cards should visually scream. This is Sean's action queue.
- **In Review** -- Agent finished work, awaiting Sean's review. Also Sean's action queue.
- **Blocked** -- Technical blocker (dependency, upstream issue).

Done and Cancelled cards collapse into an archive strip below the board with count badges: `Done (47) | Cancelled (3) -- show`.

## State Machine

```
backlog -> pending (assign to agent)
pending -> accepted | cancelled
accepted -> in-review | needs-input | blocked | done | cancelled
in-review -> accepted | done (Sean reviews -- approves or sends back)
needs-input -> accepted (Sean provides input, agent resumes)
blocked -> accepted (blocker resolved)
```

Sean can override the state machine from the dashboard (drag a card anywhere). Agents must follow it via CLI.

## Data Model Changes

Three new columns on the `tasks` table (additive migrations):

| Column | Type | Default | Purpose |
|--------|------|---------|---------|
| `parent_task_id` | TEXT | NULL | Delegation chain -- which task spawned this one |
| `labels` | TEXT | NULL | Comma-separated tags (e.g., "frontend,urgent,refactor") |
| `sort_order` | INTEGER | 0 | Global priority ordering within a column (lower = higher) |

New statuses: `backlog`, `in-review`, `needs-input`, `cancelled` (added to the existing `pending`, `accepted`, `done`, `blocked`).

`to_repo` uses sentinel value `_backlog` for unassigned tasks (avoids altering NOT NULL constraint).

Migration approach follows existing pattern in `db.rs::init_schema` using `has_column` checks.

### What We Skip in the Data Model

- **Dependencies (blocks/blocked-by):** Over-engineered for 12 agents. The `blocked` status + `note` field captures this.
- **Estimation:** Meaningless for AI agents.
- **Due dates:** AI agents don't have calendars.
- **Task history table:** Current pattern of overwriting `note` is lossy but simple. Add `task_events` later if audit trail matters.

## API Endpoints

### New Endpoints

**POST `/api/tasks/{id}/transition`** -- Universal state transition

```json
{
  "action": "accept" | "review" | "need-input" | "block" | "unblock" | "done" | "cancel" | "assign" | "reopen",
  "note": "optional note",
  "to_repo": "optional, for assign action"
}
```

Replaces per-action endpoints. Backend validates state machine. Returns updated task.

**POST `/api/tasks/{id}/move`** -- Drag-and-drop (Sean only, bypasses state machine)

```json
{
  "status": "accepted",
  "sort_order": 3
}
```

**POST `/api/tasks/{id}/reorder`** -- Priority reorder within column

```json
{
  "sort_order": 2
}
```

**PATCH `/api/tasks/{id}`** -- Update task fields

```json
{
  "text": "updated description",
  "priority": "high",
  "labels": "frontend,auth",
  "to_repo": "kelex",
  "context": "additional context"
}
```

**GET `/api/tasks`** -- Enhanced with query params

- `?status=pending,accepted` -- filter by status
- `?agent=kelex` -- filter by to_repo
- `?from=meatbag` -- filter by from_repo
- `?label=frontend` -- filter by label
- `?include_done=false` -- exclude done/cancelled (default)

**GET `/api/tasks/{id}/chain`** -- Delegation chain (task + all descendants)

**GET `/api/agents/status`** -- Agent workload summary

```json
[
  {
    "repo": "kelex",
    "active_count": 3,
    "pending_count": 1,
    "blocked_count": 0,
    "last_activity": "2026-03-21T...",
    "status": "busy"
  }
]
```

Status derived: idle (0 active tasks), blocked (all active are blocked/needs-input), busy (otherwise).

### New CLI Subcommands

```
legion task review --id <task-id>
legion task need-input --id <task-id> --reason "what do you need?"
legion task cancel --id <task-id>
legion task assign --id <task-id> --to <repo>
```

## Dashboard UI

### Card Design

```
+---------------------------------+
| [high] kelex                    |
| Implement BM25 hybrid search   |
| frontend, search               |  <- label chips
| from: meatbag  |  2h ago       |
| [Done] [Review] [Block]        |  <- quick actions
+---------------------------------+
```

- Click card to expand: full description, context, note, delegation chain
- Tree icon with count if task has children
- Quick action buttons change per status column

### Agent Status Strip (top of tasks view)

```
kelex [busy] 3 active | rafters [idle] 0 active | platform [blocked] 1 blocked
```

Color: green=idle, yellow=busy, red=blocked. Click agent name to filter board.

### Filter Toolbar

```
[Group: by status | by agent] [Filter: all agents v] [Priority: all v] [Hide done: on/off]
```

- **Group by status**: Default kanban view
- **Group by agent**: Columns are agents, cards grouped by status within each

### Drag and Drop

HTML5 drag-and-drop, vanilla JS (~80 lines):
- `draggable="true"` on cards
- Column highlight on dragover
- Optimistic UI update, snap back on API rejection
- Within-column drag for reorder

### Mobile

- Agent status strip becomes primary navigation
- Kanban columns stack vertically, each collapsible
- Quick action buttons are primary interaction (no drag-and-drop)
- CSS-only responsive, no separate codebase

## Build Phases

### Phase K1: Backend (data model + API)

1. Add `parent_task_id`, `labels`, `sort_order` columns via migration in `db.rs`
2. Update `Task` struct in `task.rs` with new fields
3. Expand state machine in `task.rs` for new statuses
4. Add `POST /api/tasks/{id}/transition` endpoint in `serve.rs`
5. Add `PATCH /api/tasks/{id}` endpoint
6. Add `GET /api/agents/status` endpoint
7. Update `get_all_tasks` with filter params, exclude done/cancelled by default
8. Add new CLI subcommands in `main.rs`
9. Tests for all new state transitions

### Phase K2: Dashboard kanban upgrade

1. Update `buildKanbanColumns` to 6 columns with new status mapping
2. Add quick action buttons to cards
3. Add card expand/collapse for full details
4. Add filter toolbar (agent, priority, hide done)
5. Add agent status strip
6. Add done/cancelled archive section
7. CSS updates

### Phase K3: Drag-and-drop + reordering

1. HTML5 drag-and-drop in `app.js`
2. Add `POST /api/tasks/{id}/move` and `/reorder` endpoints
3. Visual drag feedback CSS
4. Sort cards within columns by `sort_order`

### Phase K4: Delegation chains + advanced

1. Add `GET /api/tasks/{id}/chain` endpoint
2. Chain visualization in expanded card view
3. Group-by-agent view mode
4. Parent task selector in creation form
5. Labels as clickable filter chips

## Files to Modify

- `src/db.rs` -- Schema migrations, updated queries, agent status query
- `src/task.rs` -- Expanded state machine, new Task struct fields, chain traversal
- `src/serve.rs` -- New API endpoints
- `src/main.rs` -- New CLI subcommands
- `static/app.js` -- Kanban UI overhaul
- `static/style.css` -- New styles for expanded board

## Open Questions for Sean

1. Should agents be able to self-assign from backlog, or is all assignment through you?
2. Is group-by-agent view valuable enough for K2, or can it wait for K4?
3. Do you want notification sounds / browser notifications when cards land in Needs Input or In Review?
4. Should the kanban be the default tab instead of Feed?
