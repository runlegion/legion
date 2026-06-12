# Architecture

Legion is a local Rust binary backed by SQLite and Tantivy. This document covers the storage layout, database schema, search index, scoring algorithms, state machines, the watch daemon, multi-node cluster sync, the health system, and the web server.

## Storage layout

All data lives in one directory (macOS: `~/Library/Application Support/legion/`, Linux: `~/.local/share/legion/`, overridden by `LEGION_DATA_DIR`):

```
legion.db          SQLite database (WAL mode)
index/             Tantivy full-text search index
watch.toml         Watch daemon configuration
watch.pid          PID lock for watch daemon (created at runtime)
cluster.toml       Multi-node cluster sync config (0o600, contains secret)
```

## Database schema

SQLite via rusqlite. WAL mode is enabled on every open for concurrent read performance. The schema is created and migrated incrementally -- each migration checks for the column or table before applying.

### reflections

The core table. Stores both private reflections (audience = 'self') and bullpen posts (audience = 'team').

```sql
CREATE TABLE reflections (
    id TEXT PRIMARY KEY,                              -- UUIDv7
    repo TEXT NOT NULL,                               -- repository/agent name
    text TEXT NOT NULL,                               -- reflection content
    created_at TEXT NOT NULL,                          -- ISO 8601 timestamp
    embedding BLOB,                                   -- model2vec embedding (nullable)
    audience TEXT NOT NULL DEFAULT 'self',             -- 'self' or 'team'
    domain TEXT,                                       -- classification tag
    tags TEXT,                                         -- comma-separated tags
    recall_count INTEGER NOT NULL DEFAULT 0,           -- boost counter
    last_recalled_at TEXT,                              -- for decay calculation
    parent_id TEXT,                                     -- learning chain link
    handled_at TEXT,                                    -- legacy watch handling (superseded by watch_handled)
    archived_at TEXT,                                   -- bullpen archive timestamp (nullable)
    deleted_at TEXT,                                    -- tombstone for multi-node sync (nullable)
    updated_at TEXT                                     -- ISO 8601, LWW conflict resolution
);

CREATE INDEX idx_reflections_repo ON reflections(repo);
CREATE INDEX idx_reflections_created ON reflections(created_at);
```

### board_reads

Tracks the last time each repo read the bullpen. Used to compute unread counts.

```sql
CREATE TABLE board_reads (
    reader_repo TEXT NOT NULL PRIMARY KEY,
    last_read_at TEXT NOT NULL
);
```

### tasks

The kanban board. Stores cards for work delegation between agents. Also used by the legacy `task` subcommand (deprecated in favor of `kanban`).

```sql
CREATE TABLE tasks (
    id TEXT PRIMARY KEY,                               -- UUIDv7
    from_repo TEXT NOT NULL,                            -- creator
    to_repo TEXT NOT NULL,                              -- assignee
    text TEXT NOT NULL,                                 -- card description
    context TEXT,                                       -- additional context
    priority TEXT NOT NULL DEFAULT 'med',               -- low, med, high, critical
    status TEXT NOT NULL DEFAULT 'pending',             -- see CardStatus enum
    note TEXT,                                          -- completion note or block reason
    created_at TEXT NOT NULL,                           -- ISO 8601
    updated_at TEXT NOT NULL,                           -- ISO 8601
    labels TEXT,                                        -- comma-separated labels
    parent_card_id TEXT,                                -- delegation chain parent
    source_url TEXT,                                    -- external issue URL
    source_type TEXT,                                   -- source type (github, jira)
    sort_order INTEGER NOT NULL DEFAULT 0,             -- manual ordering
    assigned_at TEXT,                                   -- when card was assigned to an agent
    started_at TEXT,                                    -- when agent accepted the card
    completed_at TEXT,                                  -- when card reached done/cancelled
    deleted_at TEXT,                                    -- tombstone for multi-node sync (nullable)
    problem TEXT,                                       -- parsed problem statement from issue body
    solution TEXT,                                      -- parsed solution statement from issue body
    acceptance TEXT,                                    -- newline-separated acceptance criteria
    document_id TEXT                                    -- bound spec document (nullable, #528)
);

CREATE INDEX idx_tasks_to ON tasks(to_repo, status);
CREATE INDEX idx_tasks_from ON tasks(from_repo, status);
CREATE INDEX idx_tasks_parent ON tasks(parent_card_id);
CREATE INDEX idx_tasks_status_sort ON tasks(status, sort_order, created_at);
CREATE INDEX idx_tasks_document_id ON tasks(document_id) WHERE document_id IS NOT NULL;
```

`tasks.updated_at` already existed pre-sync and is reused for LWW.

### documents

Coordination substrate for specs, NFRs, blueprints, personas, journeys, and schemas (#456, #526). Type-agnostic at the storage layer -- the payload is validated JSON; the meta columns (type, surface, status, priority, owner) are hoisted out of the payload and indexed so SQL can filter without parsing JSON. Hot/cold tiered: `archived_at` is null for hot documents and populated when work referencing the doc completes.

```sql
CREATE TABLE documents (
    id TEXT PRIMARY KEY,                               -- UUIDv7 or caller-supplied typed id
    type TEXT NOT NULL,                                 -- doc_type: requirement, schema, persona, etc.
    surface TEXT,                                       -- functional surface (optional)
    status TEXT NOT NULL DEFAULT 'draft',               -- lifecycle status
    priority TEXT,                                      -- RFC 2119 level: SHALL, SHOULD, MAY
    owner TEXT NOT NULL,                                -- owning agent id
    payload TEXT NOT NULL,                              -- validated JSON payload
    archived_at TEXT,                                   -- hot/cold tier: null = hot
    created_at TEXT NOT NULL,                           -- ISO 8601
    updated_at TEXT NOT NULL,                           -- ISO 8601
    deleted_at TEXT                                     -- tombstone (nullable)
);

CREATE INDEX idx_documents_type ON documents(type) WHERE archived_at IS NULL AND deleted_at IS NULL;
CREATE INDEX idx_documents_surface ON documents(surface) WHERE archived_at IS NULL AND deleted_at IS NULL;
CREATE INDEX idx_documents_owner ON documents(owner) WHERE archived_at IS NULL AND deleted_at IS NULL;
CREATE INDEX idx_documents_status ON documents(status) WHERE archived_at IS NULL AND deleted_at IS NULL;
```

Schema documents (`doc_type=schema`) are validated structurally at insert: the payload must be a valid JSON Schema with `$schema`, `title`, `type:object`, and non-empty `properties`. Invalid schema payloads are rejected at create time. A dual-write pointer reflection (`domain=schema`) is stored so `legion recall --domain schema` surfaces every landed schema with its document id. `legion document archive` is idempotent: re-archiving preserves the original `archived_at` timestamp via COALESCE.

### schedules

Cron-like scheduled bullpen posts.

```sql
CREATE TABLE schedules (
    id TEXT PRIMARY KEY,                               -- UUIDv7
    name TEXT NOT NULL,                                -- human-readable name
    cron TEXT NOT NULL,                                -- "HH:MM" or "*/Nm"
    command TEXT NOT NULL,                              -- text to post
    repo TEXT NOT NULL,                                -- repo for the post
    enabled INTEGER NOT NULL DEFAULT 1,               -- 0 = disabled
    last_run TEXT,                                      -- ISO 8601 of last fire
    next_run TEXT NOT NULL,                             -- ISO 8601 of next fire
    created_at TEXT NOT NULL,                           -- ISO 8601
    active_start TEXT,                                  -- HH:MM UTC window start
    active_end TEXT,                                    -- HH:MM UTC window end
    deleted_at TEXT,                                    -- tombstone for multi-node sync (nullable)
    updated_at TEXT                                     -- ISO 8601, LWW conflict resolution
);
```

Cron formats:
- `HH:MM` -- fires daily at that UTC time
- `*/Nm` -- fires every N minutes from creation

Active windows: if both `active_start` and `active_end` are set, the schedule only fires within that time window. Supports overnight windows (e.g., 23:00-07:00 crosses midnight).

### watch_handled

Per-repo signal handling for the watch daemon. Replaced the global `handled_at` column on reflections for watch purposes. Allows @all broadcasts to be independently handled by each repo.

```sql
CREATE TABLE watch_handled (
    signal_id TEXT NOT NULL,
    repo_name TEXT NOT NULL,
    handled_at TEXT NOT NULL,
    PRIMARY KEY (signal_id, repo_name)
);
```

### health_samples

System telemetry for spawn gating and the health dashboard.

```sql
CREATE TABLE health_samples (
    id TEXT PRIMARY KEY,                               -- UUIDv7
    hostname TEXT NOT NULL,
    sampled_at TEXT NOT NULL,                           -- ISO 8601
    cpu_usage_pct REAL NOT NULL,
    load_avg_1 REAL,                                   -- 1-min load average (NULL on Windows)
    load_avg_5 REAL,                                   -- 5-min load average
    load_avg_15 REAL,                                  -- 15-min load average
    cpu_core_count INTEGER NOT NULL,
    mem_total_bytes INTEGER NOT NULL,
    mem_used_bytes INTEGER NOT NULL,
    mem_usage_pct REAL NOT NULL,
    swap_total_bytes INTEGER,                          -- NULL if no swap
    swap_used_bytes INTEGER,
    cpu_temp_celsius REAL,                             -- hottest CPU component (NULL if unavailable)
    agents_active INTEGER NOT NULL DEFAULT 0,         -- spawned agents currently running
    pressure REAL NOT NULL                             -- computed composite pressure (0-100)
);

CREATE INDEX idx_health_hostname ON health_samples(hostname);
CREATE INDEX idx_health_sampled ON health_samples(sampled_at);
```

## Migrations

The `init_schema` function applies migrations incrementally using `has_column` checks:

| # | Description |
|---|-------------|
| 0 | Create reflections table with id, repo, text, created_at, embedding |
| 1 | Add audience column + board_reads table |
| 2 | Add Synapse metadata: domain, tags, recall_count, last_recalled_at, parent_id |
| 3 | Create tasks table |
| 4 | Create schedules table |
| 5 | Add active_start, active_end to schedules |
| 6 | Add handled_at to reflections (legacy watch tracking) |
| 7 | Create watch_handled table (per-repo signal tracking) |
| 8 | Create health_samples table |
| 9 | Kanban upgrade: add labels, parent_card_id, source_url, source_type, sort_order, assigned_at, started_at, completed_at to tasks. Backfill timestamps from existing status. |
| 10 | Bullpen archive: add nullable archived_at on reflections (#168) |
| 11 | Audit log table for work source actions (#142) |
| 12 | Quality gates table for PR creation guard (#200) |
| 13 | Soft delete support for multi-node sync: add deleted_at on reflections, tasks, schedules (#245) |
| 14 | LWW conflict resolution: add updated_at on reflections and schedules (#255) |
| 15 | Partial indexes that skip soft-deleted rows (#256) |
| 16 | Card-spec binding: add document_id to tasks, partial index on document_id (#528) |
| 17 | SCIP indexes for code intelligence (#278); also assigned to persona wake leases (#308) -- the number collides across domain modules, the guards make it harmless |
| 18 | Bullpen post decay (#376) |
| 19 | Signal/post resolution marker (#362) |
| 20 | Agent session log (#389) |
| 21 | Documents table for the coordination substrate (#456) |
| 22 | Uncertainty engine: uncertainty_prediction + uncertainty_calibration_snapshot tables (#355) |
| 23 | wake_attempts table -- per-wake lifecycle FSM for PTY spawn tracking (#487) |
| 24 | watch_heartbeat table -- daemon liveness heartbeat (#581) |

Migration numbers above 15 are applied in the domain modules that own the corresponding DDL, not in the reflections/kanban/schedules patch sequence. All migrations are idempotent: `CREATE TABLE IF NOT EXISTS` for new tables, `has_column` guards for `ALTER TABLE` column additions.

On a fully-migrated database, `init_schema` does minimal work: `CREATE IF NOT EXISTS` checks and a few `PRAGMA table_info` queries.

## Tantivy search index

Three schema fields:

| Field | Type | Options |
|-------|------|---------|
| `id` | STRING | STORED -- exact match, retrievable |
| `repo` | STRING | exact match filtering |
| `text` | TEXT | tokenized with `en_stem` (English stemmer), `WithFreqsAndPositions` |

The index writer uses a 15MB heap budget. Writer acquisition retries up to 3 times with exponential backoff (100ms base) to handle concurrent hook access.

Index recovery: if the index directory is corrupted, legion wipes and recreates it. Run `legion reindex` afterward to repopulate from the database.

### Search modes

**Repo-scoped** (`index.search`) -- BooleanQuery combining a TermQuery on `repo` (exact match) with a parsed query on `text` (BM25). Used by `recall`.

**All-repo** (`index.search_all`) -- QueryParser on `text` only, no repo filter. Used by `consult`.

## Recall scoring

### BM25-only mode

When the embedding model is unavailable, recall uses pure BM25 with boost/decay:

```
final_score = bm25_score * boost_factor * decay_factor
```

### Hybrid mode

When the embedding model is available (model2vec-rs), recall blends BM25 and cosine similarity:

```
hybrid = 0.6 * (bm25_score / max_bm25) + 0.4 * cosine_similarity
final_score = hybrid * boost_factor * decay_factor
```

BM25 scores are normalized by dividing by the maximum BM25 score in the result set. Cosine-only candidates (no BM25 match) must exceed a minimum threshold of 0.3 to be included.

### Boost factor

```
boost_factor = 1.0 + 0.1 * recall_count
```

Each boost adds 10% to the score. A reflection boosted 5 times scores 1.5x its base score.

### Decay factor

Based on `last_recalled_at`:

| Days since last recall | Factor |
|----------------------|--------|
| 0-7 | 1.0 |
| 7-30 | Linear interpolation from 1.0 to 0.5 |
| 30-90 | Linear interpolation from 0.5 to 0.25 |
| 90-270 | Linear interpolation from 0.25 to 0.1 |
| 270+ | 0.1 (floor) |

Never recalled (NULL) returns 1.0 -- no penalty. The floor of 0.1 ensures old wisdom remains findable.

## Signal format

Signals are bullpen posts whose text starts with `@`. The parser handles several formats:

### Full format

```
@recipient verb:status {key: value, key: value} -- note
```

### Verb-only

```
@all announce: PR #85 merged
```

Parsed as: recipient=all, verb=announce, status=None, trailing="PR #85 merged"

### With details block

```
@legion review:approved {surface: cap-output, chain: confirmed}
```

Details are parsed as comma-separated `key: value` pairs within braces. Multi-word values are supported.

### Detection

`is_signal(text)` checks if the trimmed text starts with `@`. This is used by bullpen filtering (`--signals` / `--musings`) and the watch daemon.

### Note limit

Signal notes are limited to 280 bytes. The `validate_note` function returns a `SignalNoteTooLong` error that suggests using `legion post` instead.

### Compact display

Signals render as one-liners in bullpen output:

```
[kelex] @legion review:approved  (2026-03-09)
[legion] @all announce -- Phase 2.1 shipped  (2026-03-09)
```

## Kanban state machine

Eight states with enforced transitions:

```
backlog --assign--> pending --accept--> accepted --review--> in-review --done--> done
                                           |                     |
                                           +--need-input--> needs-input
                                           |                     |
                                           +--block------> blocked  <--resume-- needs-input
                                           |                     |            <--resume-- in-review
                                           +--done-------> done
                                           |
                                           +--cancel-----> cancelled
```

### Complete transition table

| Current State | Action | New State |
|--------------|--------|-----------|
| backlog | assign | pending |
| pending | accept | accepted |
| accepted | review | in-review |
| accepted | need-input | needs-input |
| accepted | block | blocked |
| accepted | done | done |
| blocked | unblock | accepted |
| needs-input | resume | accepted |
| in-review | resume | accepted |
| in-review | done | done |
| done | reopen | backlog |
| cancelled | reopen | backlog |
| any (except done) | cancel | cancelled |

Invalid transitions return `InvalidCardTransition` with the current state and attempted action.

### Dashboard labels

| Status | Dashboard Label |
|--------|----------------|
| backlog | Backlog |
| pending | Ready |
| accepted | In Progress |
| needs-input | Needs Input |
| in-review | In Review |
| blocked | Blocked |
| done | Done |
| cancelled | Cancelled |

### Card fields

| Field | Type | Description |
|-------|------|-------------|
| id | TEXT | UUIDv7 |
| from_repo | TEXT | Creator |
| to_repo | TEXT | Assignee |
| text | TEXT | Description |
| context | TEXT | Additional context (nullable) |
| priority | TEXT | low, med, high, critical |
| status | CardStatus | Current state |
| note | TEXT | Completion note or block reason (nullable) |
| labels | TEXT | Comma-separated labels (nullable) |
| parent_card_id | TEXT | Delegation chain parent (nullable) |
| source_url | TEXT | External issue URL (nullable) |
| source_type | TEXT | Source system (nullable) |
| sort_order | INTEGER | Manual ordering (default 0) |
| created_at | TEXT | ISO 8601 |
| updated_at | TEXT | ISO 8601 |
| assigned_at | TEXT | When assigned (nullable) |
| started_at | TEXT | When accepted (nullable) |
| completed_at | TEXT | When done/cancelled (nullable) |
| problem | TEXT | Problem statement parsed from issue body (nullable) |
| solution | TEXT | Solution statement parsed from issue body (nullable) |
| acceptance | TEXT | Newline-separated acceptance criteria parsed from issue body (nullable) |
| document_id | TEXT | Bound spec document id (nullable, #528) |

### Transactional status sync

When a card has a `document_id`, status transitions are synchronized to the bound document's `status` field in the same transaction. The mapping is:

| Card status reached | Document status set |
|--------------------|---------------------|
| accepted | accepted |
| in-review | implemented |
| done | verified |
| cancelled | cancelled |

Only the four terminal/review statuses trigger a sync write. Other transitions (pending, blocked, needs-input, resume) leave the document status unchanged.

If the bound document does not exist or the payload cannot be parsed as the expected spec shape, the transition fails and the card status is not changed. A dangling `document_id` is a hard error, not a silent no-op.

## Watch daemon

### Dual-interval loop

The watch daemon runs two timers:

1. **Health sampling** every `health_poll_secs` (default 5s)
   - Calls `sampler.sample()` to collect CPU, memory, swap, temperature
   - Reaps finished child processes
   - Persists the health sample to the database
   - Prunes health samples and watch_handled records older than `retention_days`

2. **Spawn checks** every `poll_interval_secs` (default 30s)
   - Gated on `sampler.can_spawn(threshold)` -- skipped if rolling pressure exceeds the threshold
   - Checks each configured repo for pending signals
   - Auto-unblocks cards when announce signals contain "completed:"
   - Spawns agents for repos with unhandled signals

The main loop sleeps 1 second between iterations.

### Configuration

All fields in `watch.toml`:

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `poll_interval_secs` | u64 | 30 | Seconds between spawn checks |
| `cooldown_secs` | u64 | 300 | Minimum seconds between wakes per repo |
| `stagger_secs` | u64 | 15 | Seconds between spawning agents (0 disables) |
| `work_hours_start` | u8 | none | Local hour (0-23) when cooldown is disabled |
| `work_hours_end` | u8 | none | Local hour (0-23) when cooldown re-enables |
| `health_threshold_pct` | f64 | 80.0 | Pressure threshold for spawn gating (0-100) |
| `health_poll_secs` | u64 | 5 | Seconds between health samples |
| `health_window_size` | usize | 6 | Number of samples in the rolling pressure window |
| `retention_days` | u64 | 7 | Days to retain health samples before pruning |
| `serve` | bool | false | Enable the web dashboard on this node |
| `repos` | array | required | List of watched repositories |

Repo config:

| Field | Type | Description |
|-------|------|-------------|
| `name` | string | Repository name (matches signal @recipient) |
| `workdir` | string | Absolute path to working directory (must exist) |

### PID lock

Only one watcher can run at a time. The lock file (`watch.pid`) contains the PID. On startup, if the file exists, the watcher checks whether the process is alive (via `kill -0`). If the process is dead, the stale lock is removed. If alive, the watcher exits with `WatchAlreadyRunning`.

The lock is released via an RAII guard (`PidLockGuard`) that removes the file on drop.

### Signal detection

`find_pending_signals(db, repo_name, since)` queries the database for team-audience reflections that:
- Start with `@` (are signals)
- Are directed at `repo_name` or `@all`
- Have not been handled for this repo (not in `watch_handled`)
- Were created after `since` (if provided)

### Agent spawning

Agents are spawned as:

```
claude --print -p "<wake prompt>"
```

In the repo's `workdir`, with `LEGION_AUTO_WAKE=1` in the environment. Stdout and stderr are redirected to `/dev/null`.

A wake fires only for signals whose `--verb` is in the wake-worthy set: `question`, `request`, `handoff`, `correction`, `proposal`, `decision`, `rfc`, `routing`. Signals with other verbs (`announce`, `ack`, `info`, `answer`) deliver to live sessions via channel push but do not spawn an asleep recipient. Posts never wake -- posts are broadcast, not direction.

When a wake fires, `build_wake_prompt` in `src/watch.rs` groups pending signals into two sections matching the same verb cut:

- **REQUIRES A REPLY** -- the wake-worthy verbs. The prompt instructs the agent that "silence on a directed question is ghosting, not acknowledgment" and that a short refusal is a valid reply but no reply is not.
- **INFORMATIONAL** -- everything else. Silence is acknowledgment; the prompt explicitly warns against empty acks like "acknowledged, no action needed" that waste tokens and trigger wake storms.

After the sections, the prompt directs the agent to use `legion signal` to reply, `legion bullpen` for broader context, and `legion reflect` to store learnings before exit.

### Auto-unblock

When the poll cycle finds signals, it calls `check_auto_unblock`. This scans for announce signals containing "completed:" and cross-references against blocked cards whose `note` mentions the completing repo. Matching cards are transitioned from blocked to accepted.

### Cooldown

Per-repo cooldown prevents wake storms. After waking a repo, no further wakes happen for `cooldown_secs` (default 300 seconds / 5 minutes). During work hours (if configured), cooldown is disabled entirely.

### Watch config subcommands

`watch.toml` can be edited in place, but `legion watch add|remove|list` manage entries without hand-editing TOML. `add` canonicalizes the path and is idempotent (repeated calls with the same resolved path are a no-op). `remove` deletes by display name. `list` prints the current entries.

```bash
legion watch add /Volumes/store/projects/acme --name acme --agent acme-bot
legion watch remove acme
legion watch list
```

The unknown-field preservation pattern (`#[serde(flatten)] toml::Table extras`) means these commands round-trip user-added fields that legion does not yet understand.

## Multi-node cluster sync

Legion nodes on the same LAN can synchronize reflections, cards, and schedules over UDP broadcast. All packets are encrypted with XChaCha20-Poly1305 using a pre-shared 256-bit key. Sync is opt-in and disabled by default.

### cluster.toml

Stored in the data directory with `0o600` permissions on Unix (contains the pre-shared secret):

```toml
enabled = true
secret = "64-hex-chars"          # 256-bit key, generated by `legion cluster init`
port = 31337                     # UDP broadcast port (default: 31337)
instance_id = "hostname"         # optional; defaults to system hostname
last_sync = "2026-04-15T00:00:00Z"
```

Unknown fields round-trip via `#[serde(flatten)] toml::Table extras`.

### Sync actor

When `cluster.enabled = true`, `legion watch` spawns a sync actor alongside the health/spawn loops. The actor discovers peers by listening on the configured UDP port, and periodically queries the local database for deltas produced since the last broadcast. Delta transmission over the wire is scaffolded -- the serialization format is shipped (ReflectionDelta, CardDelta, ScheduleDelta) but the broadcast/apply pipeline is still landing behind it.

### Soft delete + LWW

Every syncable table (reflections, tasks, schedules) carries `deleted_at` and `updated_at`:

- **Soft delete**: deletes set `deleted_at` rather than removing the row. Reads are filtered through partial indexes (migration #15) that skip tombstoned rows. This lets delete operations replicate to peers that may not have received the row yet.
- **LWW conflict resolution**: when two nodes update the same row, the higher `updated_at` wins. Clock skew is not handled -- operators are expected to run NTP.

### Delta types

`ReflectionDelta`, `CardDelta`, and `ScheduleDelta` are the wire-format shapes. Each carries the row's primary key, its `updated_at`, and the full field set. Apply is idempotent: re-applying an older delta is a no-op because the local `updated_at` already exceeds the incoming one.

### Tombstone housekeeper

`legion watch` also runs a weekly housekeeper that hard-deletes rows whose `deleted_at` exceeds the tombstone retention window (default 7 days). Housekeeping is bounded so late-joining peers still see recent deletes but the database does not grow unbounded.

### Keys and enrollment

- `legion cluster init` generates a new 256-bit key and writes `cluster.toml`. Pass `--key <hex>` to use an existing key (for the second node in a cluster).
- `legion cluster key` prints the current key so it can be shared with another node.
- `legion cluster enable|disable` toggles the `enabled` flag.
- `legion cluster status` reports the instance ID, peer list, and last successful sync.

## Health and pressure

### Pressure computation

Instantaneous pressure from a single snapshot:

```rust
fn compute_pressure(snapshot: &HealthSnapshot) -> f64 {
    let mut pressure = snapshot.cpu_usage_pct;
    pressure = pressure.max(snapshot.mem_usage_pct);       // memory takes priority if higher
    pressure = pressure.max(swap_pct);                      // swap contributes directly
    if cpu_temp > 90.0 { pressure = pressure.max(95.0); }  // thermal hard cap
    else if cpu_temp > 80.0 { pressure = pressure.max(temp); }
    pressure.clamp(0.0, 100.0)
}
```

The formula takes the worst of CPU usage, memory usage, and swap usage. No multiplier on swap -- on 16GB Macs, 70-85% swap is normal during multi-agent sessions. Thermal throttling: CPU temp above 90C forces pressure to 95%, above 80C contributes directly.

### Rolling window average

The `HealthSampler` maintains a rolling window of `health_window_size` (default 6) pressure values. The average of this window is the pressure used for spawn gating.

```
can_spawn = rolling_pressure_average < health_threshold_pct
```

If the window is empty (no samples yet), spawning is allowed by default.

### CLI display

```bash
legion health
```

Shows a gauge display:

```
CPU:  [|||||||...........]  35.2%  (4 cores)
MEM:  [||||||||||||........]  62.1%  (10.0 / 16.0 GB)
SWAP: [||||||..............]  31.4%  (2.1 / 6.8 GB)
TEMP: [|||||...............]  72.3C
LOAD: 3.21  2.84  2.56
```

Format helpers: `render_gauge(pct, width)` produces a bar of `|` and `.` characters. `format_bytes(bytes)` renders as GB or MB.

### History and JSON

```bash
legion health --history 1h       # time series for the last hour
legion health --history 24h      # last day
legion health --all-hosts        # all hosts (after replication)
legion health --json             # machine-readable output
```

## Web server

Axum-based HTTP server with embedded static assets (via `rust_embed`).

### Start

```bash
legion serve --port 3131
```

Or set `serve = true` in `watch.toml` to have the watch daemon start it.

### REST routes

| Method | Path | Description |
|--------|------|-------------|
| GET | `/` | Dashboard HTML (embedded static) |
| GET | `/{*path}` | Static assets (JS, CSS) |
| GET | `/sse` | Server-Sent Events stream |
| GET | `/api/agents` | Per-repo stats (reflection count, boost sum, team posts, last activity) |
| GET | `/api/feed` | Recent team posts (with optional `filter` query param) |
| GET | `/api/tasks` | All tasks |
| GET | `/api/stats` | Aggregate statistics |
| GET | `/api/signals` | Recent signals |
| GET | `/api/status` | Agent status summary |
| GET | `/api/needs` | Team needs |
| GET | `/api/chat` | Chat history |
| GET | `/api/schedules` | All schedules |
| POST | `/api/post` | Create a bullpen post |
| POST | `/api/done` | Mark work complete |
| POST | `/api/tasks/create` | Create a new task |
| POST | `/api/tasks/{id}/accept` | Accept a task |
| POST | `/api/tasks/{id}/done` | Complete a task |
| POST | `/api/tasks/{id}/block` | Block a task |
| POST | `/api/tasks/{id}/unblock` | Unblock a task |
| POST | `/api/boost/{id}` | Boost a reflection |
| POST | `/api/schedules/create` | Create a schedule |
| POST | `/api/schedules/{id}/toggle` | Enable/disable a schedule |

### SSE events

The SSE endpoint polls the database every 2 seconds and emits events when data changes:

| Event | Trigger | Payload |
|-------|---------|---------|
| `agents` | New reflection | JSON array of per-repo stats |
| `feed` | New reflection | JSON array of recent team posts |
| `tasks` | New/updated task | JSON array of all tasks |
| `ping` | Every 30s | `{}` (keepalive) |

The stream checks for new reflections by comparing `max(created_at)` and new tasks by comparing `max(updated_at)`. Pings fire every 15 poll cycles (30 seconds).

### Graceful shutdown

The server uses `tokio::signal` to handle SIGINT/SIGTERM for graceful shutdown via Axum's `with_graceful_shutdown`.

## Surface output

`legion surface --repo <name>` gathers cross-repo highlights:

- Up to 5 recent bullpen posts from the last 24 hours
- Up to 3 high-value cross-repo reflections (high recall_count, from other repos)
- Up to 3 recently extended learning chains from the last 24 hours
- Pending inbound kanban cards

Output format:

```
[Synapse] For kelex:
- [rafters] posted 2026-03-09: "OKLCH handles gamut mapping better" (domain: color-tokens)
- [courses] high-value: "composite rule pattern" (recalled 7x)
- Chain extended: kelex/color-tokens (latest: "refinement applied")
- [task] from platform (high): "implement search endpoint" (pending)
```

Returns an empty string when there is nothing to surface.

## Coordination substrate

### Document storage

The `documents` table (migration 21) is the persistence layer for the coordination substrate. Documents are type-agnostic at the storage layer: any document whose payload is well-formed JSON can be inserted. Meta columns (`type`, `surface`, `status`, `priority`, `owner`) are caller-supplied at insert time and indexed via partial indexes scoped to the hot pool (`archived_at IS NULL AND deleted_at IS NULL`).

Hot/cold tiering mirrors the reflections model. `legion document list` returns only hot (non-archived) documents by default. `--archived` returns only the cold partition. Archiving is idempotent: `archive_document` uses `COALESCE(archived_at, now)` so re-archiving preserves the original timestamp.

### Schema registry

Documents with `doc_type=schema` are the schema registry. They serve two roles:

1. **Structural gate at insert** -- `legion document create --doc-type schema` validates the payload before storage. The dependency-free check requires: `$schema` field, `title`, `type: object`, and at least one property in `properties`. `required` must reference only defined properties. A payload failing this check is rejected with a descriptive error; the document is not stored.

2. **Instance validation** -- `legion document validate --schema <id> --file <path>` loads a landed schema document and validates the instance against it. The validator covers: `type` (including nullable arrays), `required`, `properties`, `items`, `enum`. Each violation produces one JSON-path error; exits non-zero when there are violations.

When a schema document is created successfully, a pointer reflection is dual-written on `domain=schema` so `legion recall --domain schema` surfaces every landed schema with its document id and a human-readable summary. The canonical payload lives in the document; the reflection is the searchable pointer.

### Spec-gen pipeline

`legion spec-gen --repo <surface>` derives requirement documents from service-design inputs (#527). The `--repo` argument is the `surface` field on the source documents (e.g. "payments"), not a git repository name.

Input types: `persona`, `journey`, `blueprint`, `painmatrix`, `ecosystem` -- all non-archived documents on the specified surface. One requirement candidate is derived per `moment_of_truth` entry across all inputs. Each candidate is validated against the `requirement` schema before storage.

Idempotency key: `(traces_to, surface)`. A candidate whose `traces_to` value already exists for the surface is skipped. Re-running on unchanged input is always safe.

`traces_to` convention: `<doc_id>#moment_of_truth[.n]` where `n` is the index within the source document's `moments_of_truth` array (zero-based). Each born card is born in `Backlog` status.

Rejection classes: schema validation failure, source document not found, duplicate `(traces_to, surface)` pair.

### Card-spec binding and bind guards

`tasks.document_id` binds a kanban card to a spec document (#528). The binding is application-layer unique: `bind_card_to_document` enforces that a given `document_id` is held by at most one non-cancelled card. Attempting to bind a document already bound to another card returns a hard error.

Status transitions for bound cards are synchronized to the document's `status` field in the same transaction (see "Transactional status sync" above). A dangling `document_id` (pointing to a non-existent document) is a hard error on any transition that triggers a sync write.

### Verify AC precedence

`legion verify` determines acceptance criteria in this order:

1. If the card has a `document_id`, load the bound document and read the top-level `verification.acceptance` array from the payload. If that key is present and non-empty, it is used (source labeled `spec:<doc id>` in output).
2. Otherwise, use `tasks.acceptance` (the newline-separated criteria parsed from the issue body at sync time; source labeled `card`).

Only a dangling `document_id` -- the bound document does not exist -- is a hard error: `verify` (and the Done gate, which shares the resolver as of #644) exits non-zero rather than gating on criteria for a spec that vanished. An unparseable payload or an absent/empty `verification.acceptance` block is non-fatal and falls back to `tasks.acceptance`.

## Quality-gate chain

### GateResult enum

`GateResult { Clean, Issues }` is the canonical result type stored in the `quality_gates` table. Stored as lowercase strings (`"clean"`, `"issues"`) for SQL readability. `Display` and `FromStr` are symmetric. The enum is defined in `src/verify.rs` alongside the verify decision logic.

### The four skills

The full quality pipeline for a branch is:

1. `/legion-simplify` -- review for code quality (duplication, unnecessary abstraction, stringly-typed state). Records a `legion-simplify` gate via `legion quality-gate record`.
2. `/legion-pr-write` -- compose the PR body mapping each acceptance criterion to the change that satisfies it, with evidence. Validates the body is non-empty and non-boilerplate via `legion pr write-check`. Records a `legion-pr-write` gate.
3. `/legion-review` -- parallel dimension review (spec, correctness, quality, security) with adversarial refutation of HIGH/MED findings. Records a `legion-review` gate.
4. `/legion-verify` -- per-criterion verdict (pass/fail/uncertain + evidence). Records a `legion-verify:<card>` gate.

### PR create gate requirements

`legion pr create` requires a clean `legion-simplify` gate AND a clean `legion-pr-write` gate on HEAD before a PR may be opened. Both forcing functions must have run. `--skip-gates` bypasses both with an audit row.

### Done verify gate

`legion done --id <card>` checks for a clean `legion-verify:<card>` gate when the card has acceptance criteria. Cards with no acceptance criteria are not gated. The gate key is constructed by `verify_gate_key(card_id)` in `src/verify.rs` -- the single source of truth for both the writer (`cli::verify::handle_verify`) and the reader (`cli::kanban::handle_done`).

### ExitWith typed exits

The binary uses `LegionError::ExitWith(code)` for controlled non-zero exits. `main()` is the only site that calls `std::process::exit`; all handler code returns `Result<T>` and propagates errors up. This allows handlers to be tested without spawning a subprocess and prevents `process::exit` from bypassing destructors in test contexts.

## Source layout

The source tree is organized into carved domain modules. File counts reflect the state at 0.18.0.

| Tree | Files | Owns |
|------|-------|------|
| `src/cli/` | 17 | clap `Commands` enum + per-domain handler modules |
| `src/db/` | 19 | SQLite layer: `Database` handle, `init_schema` dispatcher, per-domain `impl` blocks + DDL |
| `src/watch/` | 7 | Watch daemon: config, locks, signals, spawn, tracker, gates, mod |
| `src/mcp/` | 4 | MCP stdio server: log, tools, notifier, mod |
| `src/kanban/` | 3 | Kanban FSM and card parsing |
| `src/worksource/` | 3 | Work source plugin bridge (issues, PRs, mod) |

Other top-level modules in `src/` are single-file (e.g. `documents.rs`, `verify.rs`, `signal.rs`, `verbs.rs`, `spec_gen.rs`). Tests live alongside the code they test in `#[cfg(test)] mod tests` blocks. Integration tests are split across `tests/integration/` (one file per domain).

## File descriptor limit

On startup, legion raises the soft file-descriptor limit to the hard limit via `rlimit::increase_nofile_limit`. macOS ships a low soft limit (often 2560) which Tantivy can exhaust when opening index segments. This is a no-op on failure.
