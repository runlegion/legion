# Legion Changelog

## 0.5.0

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
