# Legion Changelog

## 0.4.6

### MCP Server Registration (actually fixed this time)
- Claude Code reads `.mcp.json` from `.claude-plugin/`, not the plugin root -- 0.4.5 had it backwards
- Fixed `.claude-plugin/.mcp.json` to use `bash` + `bin/legion-channel` wrapper instead of bare `bun` with unexpanded `${CLAUDE_PLUGIN_ROOT}`
- Removed orphaned `plugin/.mcp.json` (root level -- never read by Claude Code)

## 0.4.5

### MCP Server Registration (broken -- see 0.4.6)
- Moved `.mcp.json` from `.claude-plugin/` to plugin root -- Claude Code only looks at the root level
- MCP command now uses `bin/legion-channel` wrapper instead of bare `bun` with `${CLAUDE_PLUGIN_ROOT}` in args
- Wrapper resolves its own path to find `channel/index.ts` -- works regardless of how Claude Code spawns the process

### Binary Path Resolution
- `setup-binary.sh` now persists the resolved binary path to `.legion-binary-path` at install time
- `bin/legion` wrapper reads `.legion-binary-path` when `CLAUDE_PLUGIN_DATA` is unavailable (MCP server context, direct invocation)
- No hardcoded paths, no fallback guessing -- single source of truth written at install time

### Tests
- Added `plugin/tests/test-bin-legion.sh` -- wrapper dispatch, priority, failure modes (7 tests)
- Added `plugin/tests/test-setup-binary-path.sh` -- path persistence on install (3 tests)
- Added `plugin/tests/test-mcp-json.sh` -- file location, format, wrapper reference (8 tests)

## 0.4.4

### MCP Server Registration
- Added `.mcp.json` to plugin root so Claude Code discovers the channel MCP server
- Without this file, legion_post, legion_reply, legion_signal, and legion_task_respond tools were invisible
- The `channel` field in plugin.json is a separate mechanism; `.mcp.json` is how plugins register MCP servers

## 0.4.3

### Structured Card Storage
- Parsed fields (problem, solution, acceptance) now stored in DB on insert, not re-parsed on every read
- Migration backfills existing cards on first run
- New `card_summary()` reads stored fields directly -- no intermediate struct reconstruction
- Removed `format_summary()` (dead code after storage change)

### Plugin Fixes
- Plugin wrapper sets `CLAUDE_PLUGIN_ROOT` from its own location -- work source sync works without env var
- Removed PATH fallback from plugin wrapper and `find_plugin` -- plugin is the install method, no fallbacks
- Stale doc comment on `find_plugin` cleaned up

## 0.4.2

### Structured Cards (#178)
- `kanban list` shows problem summary and criteria count under each card
- New `legion issue view --repo <name> --number <n>` command with parsed output
- Issue bodies parsed into structured fields (problem, solution, acceptance criteria)
- Non-template issues degrade gracefully to raw body
- UTF-8 safe truncation for multi-byte characters

### Channel Filtering (#180)
- Channel backlog only delivers signals addressed to this agent + @all blockers
- Live SSE stream has the same filter -- no more 30-40KB context floods
- `@self` posts redirect to reflect (private, not broadcast)
- Word boundary checks on @mention matching (@legion no longer matches @legion-prime)

## 0.4.1

### Context Reduction

Session startup was 14.5KB and getting truncated. Now 2.9KB.

- Session startup slimmed to last self-reflection + status only (#175)
- Removed surface, next card peek, and LEGION_HELP static text from startup hook
- Status YOUR WORK section shows task count instead of listing every task individually
- `recall --latest` now filters to self-audience only (no team posts from other agents)
- Removed recall-first PreToolUse hook (fired on every Grep/Glob/WebFetch/WebSearch)
- Plugin installs legion instructions to `~/.claude/CLAUDE.md` on setup -- loaded once by Claude Code, not per tool call

### Internals
- setup-binary.sh restructured: binary download failures no longer skip CLAUDE.md setup
- CLAUDE.md injection is idempotent (HTML comment markers)

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
