# Legion Changelog

## 0.6.4

### Bug Fixes
- **`data_dir()` default is macOS-native again** (`src/main.rs`): a prior change preferred the `CLAUDE_PLUGIN_DATA` env var (set by Claude Code when running a plugin) over `ProjectDirs`, which on macOS put legion's authoritative state at `~/.claude/plugins/data/legion-legion/` instead of the native `~/Library/Application Support/legion/`. The result was split-brain: the CC plugin context wrote to the plugin data dir while bare CLI invocations and long-running `legion watch` processes wrote to the ProjectDirs path. Two divergent databases, invisible to each other, silently broke the agent learning loop -- reflections filed in one session were unreadable by sessions that opened the other file. The integrity of a single authoritative data dir is load-bearing for legion's memory, and this change makes that non-negotiable in the code: `data_dir()` now resolves to `LEGION_DATA_DIR` if set (explicit test override) or `ProjectDirs::from("","","legion").data_dir()` otherwise. `CLAUDE_PLUGIN_DATA` is no longer consulted. A permanent comment in `data_dir()` documents why and warns future agents not to reintroduce a second default.
- **Migration direction flipped** (`src/main.rs`): `migrate_from_legacy` is renamed `migrate_from_plugin_data_dir` and now copies state FROM the plugin data dir TO the ProjectDirs target. This is the reverse of the prior migration direction, which was the one that caused the split-brain in the first place. The flipped direction acts as a one-time recovery path for any user whose plugin data dir has their authoritative legion state but whose ProjectDirs path is empty. The underlying `migrate_between` copy helper is unchanged -- only the caller and its resolution of source vs target.
- **Migration skipped when `LEGION_DATA_DIR` is set** (`src/main.rs`): previously, any call to `data_dir()` with an explicit `LEGION_DATA_DIR` (tests, CI, scripts) still ran the migration against whatever source existed on the host machine. This meant integration tests that expected a clean tempdir could inherit content from the tester's real plugin data dir, producing unpredictable failures that depended on the host's state. The guard skips migration whenever `LEGION_DATA_DIR` is set because the override is the explicit "I know what I'm doing, stay out of the filesystem" signal. Added integration test `data_dir_override_suppresses_migration` that verifies the guard: spawns `legion stats` with `LEGION_DATA_DIR` pointing at a fresh tempdir and asserts stderr does not contain `"first-run migration"`. This also fixes several pre-existing environmental integration test failures (`bullpen_count_*`, `consult_*`, `quiet_by_default`, `reindex_rebuilds_from_database`, `stats_on_empty_db`, `surface_empty_database`) that were silently relying on Sean's specific host filesystem state to pass or fail.

## 0.6.3

### Bug Fixes
- **Hook PATH self-heal** (`plugin/hooks/*.sh`): Claude Code plugin hooks run in subshells that do not inherit the plugin `bin/` directory on PATH -- only the Bash tool does. Phase D hooks called bare `legion` which failed silently with "command not found", causing SessionStart recall context to vanish with no visible error. Each hook now invokes `"${CLAUDE_PLUGIN_ROOT}/bin/legion"` via a `LEGION` variable set at the top of the script. `CLAUDE_PLUGIN_ROOT` is already exported to hook processes per the plugins-reference spec, so this is the documented way to reach plugin-bundled binaries from hooks. Closes #204.
- **plugin.json MCP server registration** (`plugin/.claude-plugin/plugin.json`): Phase D declared the MCP server under a non-standard top-level `channel` field that the Claude Code plugin loader does not read. Result: zero `mcp__legion__*` tools in any CC session even though `src/mcp.rs`'s `run_stdio_loop` is fully wired. Replaced with a proper `mcpServers` key per the [plugins-reference spec](https://code.claude.com/docs/en/plugins-reference). Uses `${CLAUDE_PLUGIN_ROOT}/bin/legion` as the command so MCP server spawning resolves regardless of PATH. Does not add a `channels` array or declare the `experimental.claude/channel` capability -- the full channel push pipeline is tracked in #205 as separate feature work. This fix only restores the tool surface Phase D built and shipped dark.
- **`legion daemon --mcp` is now stdio-only** (`src/daemon.rs`): previously the `--mcp` flag ADDED an MCP stdio task alongside the HTTP server and watch loop, making the whole daemon a singleton that could not be spawned per Claude Code session. With plugin.json now correctly registering the MCP server, every CC session would otherwise spawn its own daemon subprocess that tried to bind port 3131 (failing after the first) and ran its own watch loop (competing for the PID lock and triggering recursive agent spawns). Redefined `--mcp` to mean "stdio-only": the new `run_mcp_stdio_only` path runs just the MCP stdin reader with a local broadcast sender, no HTTP bind, no watch. `legion daemon` without `--mcp` still runs the full HTTP + watch singleton for the dashboard and auto-wake. New integration test `daemon_mcp_mode_is_stdio_only` binds a port before spawning the daemon with `--port` set to that port, then verifies the subprocess exits successfully, returns a valid MCP initialize response on stdout, and never logs HTTP server startup or watch activity on stderr.

## 0.6.2

### Phase D daemon post-merge review fixes

Addresses findings from the pr-review-toolkit run on #201 that were deferred to ship the merge. Closes #202.

### Bug Fixes
- **Daemon task supervision** (`src/daemon.rs`): `run_daemon_async` now races the HTTP server, watch task, and MCP task via `tokio::select!`. Any task exiting (success, error, or panic) triggers the others to stop. Previously background task failures were silently ignored until SIGINT, so a crashing watch loop or panicking MCP handler would leave the daemon running in a half-broken state.
- **SIGINT hang on MCP stdin** (`src/daemon.rs`): `run_daemon` now calls `runtime.shutdown_timeout(Duration::from_secs(2))` after `block_on` returns, giving the blocking MCP stdin thread up to 2 seconds to exit cleanly. Without this, a `spawn_blocking` task parked on `read_until()` held the OS thread alive and blocked process exit.
- **`shutdown_signal` ctrl_c install failure now parks instead of returning** (`src/daemon.rs`): a return in the `ctrl_c` arm would cause the outer `select!` to fire immediately on daemon startup, shutting the daemon down. The failure branch now logs and parks via `std::future::pending()`.
- **MCP tool errors use `isError: true`** (`src/mcp.rs`): per MCP 2024-11-05 spec, tool execution failures go in the success envelope with `isError: true`, not as JSON-RPC `-32603` responses. JSON-RPC errors are reserved for protocol-level failures (parse errors, method not found, invalid request envelope). Added the `tool_error` helper and three tests asserting the correct envelope shape for unknown tools and `McpInvalidArgument` errors.
- **MCP error messages are sanitized** (`src/mcp.rs`): non-argument errors show `"internal error: <msg>"` instead of leaking file paths or DB internals. `McpInvalidArgument` still shows the full validation message since it is a contract error for the caller.
- **`api_post` uses `board::post_from_text_with_meta`** (`src/channel.rs`): matches the MCP handlers and propagates index failures as 500s instead of silently swallowing them with an `eprintln!`. A post that cannot be indexed is unsearchable, which is a half-broken state -- callers should see the failure and retry.
- **SSE broadcast `Lagged`/`Closed` handled explicitly** (`src/channel.rs`): previously the `Ok(_) = rx.recv()` select arm used a refutable match that silently did not fire on `Lagged(n)` (subscriber fell behind the ring buffer) or `Closed` (sender dropped). `Lagged` now logs the dropped-event count and forces a DB re-read to catch up; `Closed` ends the stream. Added two tests guarding the `TryRecvError::Lagged` and `TryRecvError::Closed` paths.

### Features
- **Embed model absence warning at daemon startup**: logs `"note: embed model not loaded -- posts via /api/post and MCP will not be similarity-searchable until card 019d7991-2eab lands"` so operators are not surprised when daemon-side posts do not appear in cosine recall results.
- **TODO comments on each `post_from_text_with_meta` call site** (`src/mcp.rs`, `src/channel.rs`): reference card `019d7991-2eab` for the embed model threading follow-up.

## 0.6.1

### Phase D: Channel + Watch + MCP unified daemon

Ports `plugin/channel/*.ts` (TypeScript + Bun) to three tokio tasks in one Rust process under a new `legion daemon` subcommand. Kills the Bun runtime dependency entirely. Closes #200.

### Features
- **`legion daemon` subcommand**: one process hosts the channel (SSE pub/sub + HTTP endpoints at `/api/feed`, `/api/tasks`, `/api/post`, `/sse`), an optional MCP stdio server (`legion daemon --mcp`), and the watch poll loop that was previously a separate `legion watch` process
- **`src/channel.rs`**: SSE broadcast channel, HTTP endpoints matching the legacy JSON shapes so existing consumers (dashboard, any external tooling that scraped the legacy endpoints) continue to work. Opens DB once per SSE subscriber lifetime and queries only on broadcast notifications, not on every tick.
- **`src/mcp.rs`**: hand-rolled JSON-RPC 2.0 stdio server. Implements `initialize`, `tools/list`, `tools/call` per MCP 2024-11-05 spec. Four legion tools: `legion_post`, `legion_reply`, `legion_signal`, `legion_task_respond`. Each handler calls existing Rust primitives (`board::post_from_text_with_meta`, `sig::*`, `task::*`) instead of re-implementing. Bounded stdin buffer (1 MB per line) to prevent resource exhaustion. UTF-8 safe response truncation.
- **`src/daemon.rs`**: task orchestration; each handler opens its own DB connection consistent with the CLI pattern (follow-up card 019d7991-2eb4 tracks deduplication between channel.rs and serve.rs). Watch task holds a `watch::PidLockGuard` for RAII cleanup on SIGINT/abort. Graceful shutdown via `select!` races HTTP server, watch task, and MCP task -- any task exiting triggers the others to stop. Ctrl+C install failure uses `pending()` instead of returning, preventing spurious instant shutdown.
- **New error variant**: `LegionError::McpInvalidArgument(String)` replaces the `LegionError::Io` abuse that was previously used for MCP argument validation errors.

### Deleted
- `plugin/channel/index.ts`, `sse-client.ts`, `event-bridge.ts`, `instructions.ts`, `types.ts`, `tools.ts`, `fakechat.ts`
- `plugin/channel/package.json`, `bun.lock`, `tsconfig.json`, `node_modules/`
- `plugin/bin/legion-channel` (Bun dispatcher wrapper)
- Bun references from `plugin/hooks/session-start.sh` and `plugin/hooks/setup-binary.sh`

### Bug Fixes (from simplify review of the port)
- **`truncate` UTF-8 safety**: byte-slicing replaced with `char_indices` iteration. Previously panicked on multibyte content, which is common in agent-generated text.
- **Watch PID lock leak on SIGINT**: `run_watch_task` now holds a `PidLockGuard` whose `Drop` impl releases the lock. Previously `watch_handle.abort()` orphaned the PID file, blocking the next daemon start.
- **Silent `"unknown"` repo fallback in MCP handlers**: removed. Required fields now return `McpInvalidArgument` errors matching the tool input schema contract.
- **`build_feed_json` silent failures**: return type changed from `Result<_, ()>` to `Result<_, LegionError>`. DB and serde errors are now propagated and logged instead of swallowed.
- **`open_db` / `open_index` error logging**: errors are now logged server-side before being converted to a generic 500 response (avoids leaking internals while preserving debuggability).
- **Dead code removed**: `is_relevant_backlog` + `starts_with_mention` in channel.rs (6 tests + 30 LOC) had `#[allow(dead_code)]` and a comment claiming MCP backlog delivery usage that never existed.
- **`build_app` passthrough deleted**: was a one-line function with a misleading "merge routers" comment that didn't do any merging.
- **`shutdown_signal` no longer panics**: `.expect()` on Ctrl+C / SIGTERM handler install replaced with logged fallback.

### Follow-up cards filed from the simplify review
- `019d7991-2eab` HIGH: daemon does not load the embedding model, so daemon-side posts (via `api_post` or MCP tools) land with NULL embeddings, degrading hybrid recall for them. Fix is non-trivial (threading `Arc<Option<EmbedModel>>` through ChannelState + MCP context).
- `019d7991-2eb4` MED: `FeedItem` / `build_feed_json` / `json_error` / `open_db` / `open_index` duplicated between `channel.rs` and `serve.rs`. Extract shared helpers.
- `019d7991-2ebc` MED: `daemon::run_watch_task` duplicates `watch::run` with logic drift already visible. Refactor to one shared implementation.

## 0.6.0

### Features
- **Kanban CLI completeness**: `legion kanban view --id <uuid>` (with `--json`), `legion kanban update --id <uuid> --text/--body/--priority/--labels/--add-labels/--remove-labels`, and `legion kanban list --json` for JSONL bulk-scan output. Unblocks the orchestrator agent's First Steps and makes grooming clean.
- **`legion pr close --repo X --number N`**: close a PR without merging via the work source. `--reason "text"` posts the reason as a closing comment. `--delete-branch` removes the remote branch after closing.
- **`legion usage` subcommand**: parses `~/.claude/projects/<slug>/<session-uuid>.jsonl` to aggregate token usage per session. Computes effective tokens (UI-style weighted total) and dollar cost (API-list pricing). Flags: `--session`, `--today`, `--since`, `--by-session`, `--by-repo`, `--json`. Pure local, no network, no DB writes.
- **Embedding reads**: `legion similar --id <uuid>` returns nearest neighbors of a reflection by cosine similarity (with `--limit`, `--cross-repo`, `--min-score`, `--preview`, `--json`). `legion recall --cosine-only` skips BM25 for pure semantic ranking. `legion recall --min-score <f32>` drops weak matches. `legion reflect --dedupe-mode warn|strict|off` detects near-duplicates on store (cosine >= 0.95 against last 100 same-repo reflections). `--force` bypasses the check.
- **`legion-simplify` skill + quality gate**: new plugin skill at `plugin/skills/legion-simplify/SKILL.md` reviews the branch diff and emits structured JSON. New `quality_gates` DB table records skill results unforgeably (written by the skill runner, not the agent). `legion pr create` refuses to open a PR without a clean simplify gate for HEAD. `--skip-gates` bootstrap flag writes an audit entry.

### Bug Fixes
- **Dispatcher fallback when CLAUDE_PLUGIN_DATA is unset**: Claude Code subagents spawned via the Task tool get a clean shell environment without `CLAUDE_PLUGIN_DATA`. The dispatcher script at `plugin/bin/legion` now defaults `DATA_DIR` to `$HOME/.claude/plugins/data/legion-legion/` when the env var is missing. Unblocks the entire team architecture -- every `.claude/agents/*.md` specialist can now run `legion recall` / `reflect` / `consult` in their First Steps.

### Dev Workflow
- CLAUDE.md updated with `legion-simplify` gate requirement on `legion pr create`
- `legion quality-gate record` added to the command reference

## 0.5.0

### Recovery Release
Rolled back the v0.4.3..v0.4.7 spiral (8 commits of `.mcp.json` location churn that chased a Claude Code 2.1.97 regression without fixing the actual bug). Cherry-picked the good halves of v0.4.1 / v0.4.2 / v0.4.3 on top of v0.4.0 and added four root-cause fixes.

### Features
- **`legion kanban` structured cards**: parsed problem/solution/acceptance fields stored on insert via `card_parse::parse_issue_body` (from v0.4.3 good-half cherry-pick)
- **Channel backlog filtering**: SSE stream delivers only `@me` signals and `@all` blockers; `@self` posts redirect to reflect instead of broadcasting (from v0.4.2 cherry-pick)
- **`legion issue view`**: reads a GitHub issue body into structured fields
- **`status --json` flag**: counts-only summary for hook output
- **PreToolUse recall-first hook restored**: v0.4.1 removed it; v0.5.0 brings it back AND upgrades it from nudge-only to execute-and-inject (actually runs `legion recall` on the tool's search query)
- **SessionStart slim**: from 14.5 KB to ~800 bytes (removed LEGION_HELP prose, surface, work peek)
- **Data dir consolidation**: transparent one-way migration from `~/Library/Application Support/legion/` (macOS ProjectDirs) to `~/.claude/plugins/data/legion-legion/` with WAL-safe SQLite copy
- **Recall `--preview N`**: truncate each hit to N characters via `card_parse::truncate_chars`
- **No-bullshit Stop hook**: blocks responses that stop mid-task to check in ("three files down, want me to continue...")
- **No-direct-db PreToolUse Bash hook**: denies any command referencing `legion.db` directly
- **Starter agent team**: six specialists at `.claude/agents/` -- orchestrator (Haiku), rust/reviewer/dashboarder/porter/issue-writer (Sonnet)

### Bug Fixes
- **`find_plugin` three-tier fallback**: restores exe-relative lookup + adds glob over `~/.claude/plugins/cache/*/legion/*/worksources/` picking highest semver. Survives `CLAUDE_PLUGIN_ROOT` being empty in Bash subprocess context. Fixes `legion pr create` "plugin not found: github" error for every agent.
- **Channel mark-as-seen**: new atomic `get_and_mark_unread_board_posts` DB transaction + `unread_for=<repo>` query param on `/api/feed` + `sse-client.ts` uses it. Race-proof via single-timestamp upper bound. Fixes "same 50 signals every connection" bug.
- **Format for hook uses `Cow<str>`**: avoids per-reflection allocation when `preview` is None

## 0.4.0

### Features
- Bullpen archive: `legion bullpen --archive` moves read posts out of the active board (#168)
- `legion bullpen --archived` views archived posts for forensics
- Archived posts remain searchable via `consult` and `recall`

### Bug Fixes
- Work source sync now preserves GitHub issue creation date instead of using insertion time (#171)
- Scheduler correctly prioritizes older issues first
- Windows stack overflow: spawn main thread with 8MB stack to match macOS/Linux defaults
- `--repo` no longer required for `--archive` and `--archived` (global operations)

### Dev Workflow
- Added 10-step dev workflow to CLAUDE.md (plan, issue, build, simplify, PR, automated review, fix, team review, consensus, ask for merge)
- Team review: vault validates spec, smugglr reviews Rust content

## 0.3.0

- Add `--version` flag to the legion binary
- setup-binary.sh reads version from plugin.json dynamically -- binary auto-updates on version bump
- One version number for binary and plugin (Cargo.toml = plugin.json)
- Add work source workflow guide to SessionStart hook context
- Slim Stop hook: just a reflect prompt, no checklist
- GitHub Release workflow on `v*` tags builds linux-x64, macos-x64, macos-arm64, windows-x64 with SHA-256 checksums
