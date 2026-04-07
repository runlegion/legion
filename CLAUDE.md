# CLAUDE.md

## What This Is

Legion is a local Rust binary that stores and retrieves agent reflections. It's the memory layer for Claude Code agents that work on specific codebases.

## Commands

```bash
legion reflect --repo <name> --text "reflection text"
legion reflect --repo <name> --transcript /path/to/transcript.jsonl
legion reflect --repo <name> --text "..." --domain color-tokens --tags "semantic,consumer"
legion reflect --repo <name> --text "..." --follows <parent-id>
legion recall --repo <name> --context "what I'm working on"
legion consult --context "problem outside your domain" --limit <n>
legion post --repo <name> --text "share with the team"
legion bullpen --repo <name>               # aliases: bp, board
legion bullpen --count --repo <name>
legion bullpen --repo <name> --signals     # show only signals
legion bullpen --repo <name> --musings     # show only musings
legion signal --repo <name> --to <recipient> --verb <verb> --status <status>
legion signal --repo <name> --to all --verb announce --note "PR merged"
legion signal --repo <name> --to <recipient> --verb review --status approved --details "surface:cap-output,chain:confirmed"
legion boost --id <reflection-id>
legion chain --id <reflection-id>
legion surface --repo <name>
legion stats --repo <name>
legion reindex
legion work --repo <name>                    # get next card from scheduler (auto-accepts)
legion work --repo <name> --peek             # peek at next card without accepting
legion done --repo <name> --text "what was completed" --id <card-id>  # complete card + announce
legion kanban create --from <repo> --to <repo> --text "description" --priority <low|med|high|critical>
legion kanban create --from <repo> --to <repo> --text "..." --labels "tag1,tag2" --source-url "https://..."
legion kanban list --repo <name>             # inbound cards (assigned to repo)
legion kanban list --repo <name> --from      # outbound cards (created by repo)
legion kanban accept --id <card-id>          # pending -> in-progress
legion kanban block --id <card-id> --reason "blocker description"
legion kanban unblock --id <card-id>         # blocked -> in-progress
legion kanban review --id <card-id>          # in-progress -> in-review
legion kanban need-input --id <card-id>      # in-progress -> needs-input (blocked on human)
legion kanban resume --id <card-id>          # needs-input/in-review -> in-progress
legion kanban cancel --id <card-id>          # any active -> cancelled
legion kanban assign --id <card-id> --to <repo>  # backlog -> assigned to agent
legion kanban reopen --id <card-id>          # done/cancelled -> backlog
legion issue create --repo <name> --title "..." --body "..."  # create issue via work source
legion pr create --repo <name> --title "..." --body "..."    # create PR via work source
legion pr list --repo <name>                 # list open PRs with review status
legion pr review --repo <name> --number <n> --approve --body "LGTM"  # post review
legion pr review --repo <name> --number <n> --request-changes --body "..."
legion pr merge --repo <name> --number <n>   # merge approved PR (refuses if not approved)
legion pr merge --repo <name> --number <n> --task <card-id>  # merge + transition card to done
legion comment --repo <name> --number <n> --body "..."       # comment on issue or PR
legion audit                                 # view recent audit log entries
legion audit --repo <name>                   # filter by agent
legion audit --action create-pr              # filter by action type
legion audit --json                          # output as JSON
legion watch                                 # auto-wake sleeping agents on signal arrival
legion -v <command>                          # show informational messages (quiet by default)
```

### Cross-Agent Consultation

When you encounter a problem outside your domain, use `legion consult` to search
reflections from ALL repos/agents:

```bash
legion consult --context "discriminated unions in composite rules" --limit 3
```

This searches across all indexed agent reflections regardless of repo (e.g., kelex, rafters, platform)
using BM25. Results include source repo attribution so you know which domain
the knowledge came from. Use this when you hit something unfamiliar -- another
agent may have already solved it.

## Architecture

- **Storage**: SQLite via rusqlite (XDG data dir: ~/.local/share/legion/)
- **Search**: Tantivy BM25 full-text index (Phase 1)
- **Future**: model2vec-rs embeddings in nullable BLOB column (Phase 2)

## Rules

- No emoji in code, comments, or documentation
- No `unwrap()` in production code
- No `unsafe` code
- All types explicit
- `cargo clippy -- -D warnings` must pass
- `cargo fmt -- --check` must pass
- Errors use thiserror derive macros
- UUIDv7 for all IDs

## Data Model

```sql
CREATE TABLE reflections (
    id TEXT PRIMARY KEY,           -- UUIDv7
    repo TEXT NOT NULL,            -- repository name
    text TEXT NOT NULL,            -- the reflection
    created_at TEXT NOT NULL,      -- ISO 8601
    audience TEXT NOT NULL DEFAULT 'self',  -- 'self' or 'team'
    domain TEXT,                   -- classification tag (e.g., "color-tokens")
    tags TEXT,                     -- comma-separated tags
    recall_count INTEGER NOT NULL DEFAULT 0,  -- boost counter
    last_recalled_at TEXT,         -- for decay calculation
    parent_id TEXT,                -- learning chain link
    embedding BLOB                 -- nullable, Phase 2.5
);

CREATE INDEX idx_reflections_repo ON reflections(repo);
CREATE INDEX idx_reflections_created ON reflections(created_at);
```

### Tasks (Agent Delegation)

Delegate work between agents with state-tracked tasks:

```bash
legion task create --from kelex --to legion --text "implement BM25 search" --priority high
legion task list --repo legion              # see inbound tasks
legion task list --repo kelex --from        # see outbound tasks
legion task accept --id <task-id>
legion task done --id <task-id> --note "shipped"
legion task block --id <task-id> --reason "waiting on upstream"
legion task unblock --id <task-id>
```

State transitions: pending -> accepted -> done|blocked. Blocked -> accepted via unblock. Invalid transitions are rejected.
Pending inbound tasks are included in `legion surface` output.

### Bullpen (Push-Based Communication)

Push something to the team instead of keeping it to yourself:

```bash
legion post --repo rafters --text "OKLCH bet paid off"
legion bullpen --repo kelex            # read all posts, mark as read
legion bullpen --count --repo kelex    # unread count only (for hooks)
```

Posts are reflections with `audience = 'team'`. Discoverable via `consult` for free.

### Signals (Structured Coordination)

Signals are compact, structured bullpen posts for coordination. Format: `@recipient verb:status {details}`.

```bash
legion signal --repo kelex --to legion --verb review --status approved
legion signal --repo kelex --to all --verb announce --note "Phase 2.1 shipped"
legion signal --repo kelex --to platform --verb request --status help --details "topic:embeddings"
```

Signals are bullpen posts whose text starts with `@`. Filter on read:
- `legion bullpen --repo <name> --signals` shows only signals
- `legion bullpen --repo <name> --musings` shows only natural language posts
- `legion bullpen --repo <name>` shows everything (signals render as compact one-liners)

### Watch (Auto-Wake)

Monitor the bullpen and automatically spawn agent sessions when signals arrive for idle agents:

```bash
legion watch
```

Reads config from `<data-dir>/watch.toml`:

```toml
poll_interval_secs = 30
cooldown_secs = 300
stagger_secs = 15       # seconds between spawns; 0 disables

[[repos]]
name = "rafters"
workdir = "/Volumes/store/projects/rafters-studio/rafters"
```

Opt-IN per repo. Only repos listed in `watch.toml` get auto-woken. PID lock prevents multiple watchers.
Cooldown prevents wake storms (default 5 minutes between wakes per repo).
Stagger prevents I/O storms by sleeping between spawns (default 15s; set to 0 to disable).

## Phase Plan

1. **Phase 1** (complete): SQLite + Tantivy BM25. Store reflections, recall by text similarity.
2. **Phase 1.5** (complete): Cross-agent consultation via `legion consult`. BM25 search across all repos.
3. **Phase 1.75** (complete): Bullpen. `legion post` and `legion bullpen` for push-based agent communication.
4. **Phase 2.0** (complete): Synapse metadata. Domain/tags, learning chains, boost/decay ranking, `legion surface`.
5. **Phase 2.1** (complete): Signals. Structured coordination via `@recipient verb:status {details}`. Bullpen filtering (`--signals`, `--musings`).
6. **Phase 2.2** (complete): Watch. Auto-wake sleeping agents when signals arrive. `legion watch` with opt-in config.
7. **Phase 2.5** (next): Add model2vec-rs embeddings, hybrid BM25 + cosine scoring, transfer detection.
8. **Phase 3.0** (planned): LLM classification via Synapse agent for quality gating.

## Hook Integration

Legion is called by Claude Code hooks:
- `SessionStart` hook calls `legion recall` + `legion surface` and injects context via additionalContext
- `Stop` hook prompts the agent to reflect before closing
- `consult` is agent-initiated (called via Bash mid-session), not hook-driven
- `post` is agent-initiated (when agent has something worth sharing with the team)
- `bullpen` is agent-initiated (when agent wants to read what others posted; `--signals`/`--musings` for filtering)
- `signal` is agent-initiated (structured coordination: reviews, requests, announcements)
- `boost` is agent-initiated (after recalling and successfully applying a reflection)
- `chain` is agent-initiated (to trace a learning chain)
- `task` is agent-initiated (delegate work to other agents, check task status)
- `watch` is operator-initiated (long-lived process that auto-wakes agents on signal arrival)

## Project Layout

```
src/
  main.rs          -- CLI entry point (clap)
  db.rs            -- SQLite init, migrations, CRUD
  search.rs        -- Tantivy index management
  reflect.rs       -- Reflection creation (from text or transcript)
  recall.rs        -- Query and rank reflections (weighted by boost/decay)
  board.rs         -- Bullpen: post, bullpen, and bullpen filtering
  signal.rs        -- Signal parsing, formatting, and detection
  surface.rs       -- Cross-repo highlight surfacing
  task.rs          -- Task delegation between agents
  watch.rs         -- Auto-wake: poll for signals, spawn agent sessions
  health.rs        -- System health sampling, pressure calculation, CLI display
  stats.rs         -- Reflection statistics reporting
  init.rs          -- Hook script generation and settings.json management
  error.rs         -- Error types
  testutil.rs      -- Shared test helpers (#[cfg(test)] only)
tests/
  integration.rs   -- End-to-end binary tests
docs/
  plans/           -- Design documents
plugin/
  .claude-plugin/  -- Plugin manifest (plugin.json)
  bin/legion       -- Wrapper script dispatching to cached binary
  hooks/           -- SessionStart, Stop, PreCompact, PreToolUse hooks
  commands/        -- Slash command skills (reflect, recall, boost, etc.)
  agents/          -- Agent definitions (legion-prime, dungeon-master)
  skills/          -- Skill definitions (legion-memory)
  channel/         -- MCP server for real-time team channel (TypeScript/Bun)
install.sh         -- Standalone installer (curl|bash) for non-plugin users
```
