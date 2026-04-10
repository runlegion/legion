# Legion Changelog

## 0.6.1

### Phase D: Channel + Watch + MCP unified daemon

Ports `plugin/channel/*.ts` (TypeScript + Bun) to three tokio tasks in one Rust process under a new `legion daemon` subcommand. Kills the Bun runtime dependency entirely. Closes #200.

### Features
- **`legion daemon` subcommand**: one process hosts the channel (SSE pub/sub + HTTP endpoints at `/api/feed`, `/api/tasks`, `/api/post`, `/sse`), an optional MCP stdio server (`legion daemon --mcp`), and the watch poll loop that was previously a separate `legion watch` process
- **`src/channel.rs`** (~532 lines): SSE broadcast channel, HTTP endpoints matching the legacy TS JSON shapes byte-for-byte so existing consumers (dashboard, sse-client, any external tooling) continue to work through the transition. Opens DB once per SSE subscriber lifetime and queries only on broadcast notifications, not on every tick.
- **`src/mcp.rs`** (~700 lines): hand-rolled JSON-RPC 2.0 stdio server. Implements `initialize`, `tools/list`, `tools/call` per MCP 2024-11-05 spec. Four legion tools: `legion_post`, `legion_reply`, `legion_signal`, `legion_task_respond`. Each handler calls existing Rust primitives (`board::post_from_text_with_meta`, `sig::*`, `task::*`) instead of re-implementing. Bounded stdin buffer (1 MB per line) to prevent resource exhaustion. UTF-8 safe response truncation.
- **`src/daemon.rs`**: task orchestration with shared `Arc<Database>`. Watch task holds a `watch::PidLockGuard` for RAII cleanup on SIGINT/abort. Graceful shutdown via `select!` on Ctrl+C + SIGTERM (with logged fallback if signal handler install fails instead of panicking).
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
