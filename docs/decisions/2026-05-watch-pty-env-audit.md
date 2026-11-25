# Watch PTY env-var + stop-hook audit

**Date:** 2026-05-23
**Issue:** #492 (audit phase of the #495 PTY migration arc)
**Status:** Audit phase complete. Implementation phase pending operator sign-off + #489 landing.

## Why this audit exists

The PTY migration (#495) changes two background assumptions:

1. `LEGION_AUTO_WAKE=1` was framed as the "non-interactive watch wake" marker. Under PTY it is set on a *fully interactive* `claude` REPL that happens to be watch-spawned. Hooks that branch on it expecting `claude --print -p` semantics may behave wrong.
2. `plugin/hooks/stop.sh` runs two blocking gates on every Stop. Under `--print -p` the gates rarely fire (the agent exits cleanly). Under PTY, every wake exits through the same Stop hook the human-attended session uses -- including the incomplete-task block. The 8-block stop-hook cap in Claude Code 2.1.143 means a stuck gate gets the agent hard-killed.

This document enumerates every site touching `LEGION_AUTO_WAKE` and decides the behavior under `LEGION_SPAWN_SOURCE=watch-pty` (the new marker `#489` will set).

## Audit scope

Searched repo-wide for `LEGION_AUTO_WAKE`, `AUTO_WAKE`, `auto-wake`, and `auto.wake` in `src/`, `plugin/`, and `docs/`. Cross-checked with `legion sym refs` (no SCIP hits for the env-var literal because it lives in string form).

Result: the env var is set in **one** place and read in **zero** places inside legion code today. External tooling (Claude Code internals, future operator scripts) may inspect it, but those are out of scope for this audit.

## Findings

### Site 1: `src/watch.rs:796`

```rust
.env("LEGION_AUTO_WAKE", "1")
```

**Today:** the watch daemon sets `LEGION_AUTO_WAKE=1` on every spawned child (`claude --print -p`).
**Under watch-pty:** the PTY-spawned `claude` interactive REPL inherits the same env-var stamp via the new `LEGION_SPAWN_SOURCE=watch-pty` marker added in `#489`. The existing `LEGION_AUTO_WAKE=1` is kept for backwards compatibility -- external tooling outside legion's repo may still branch on it, and removing it would silently break those callers.
**Decision:** keep `LEGION_AUTO_WAKE=1` as-is; add `LEGION_SPAWN_SOURCE=watch-pty` alongside in the PTY branch.
**Recommended change (in #489):** the PTY branch of `spawn_agent` sets both env vars; the `claude --print -p` branch sets only `LEGION_AUTO_WAKE` (existing behavior).

### Site 2: hooks under `plugin/hooks/`

Zero hooks read `LEGION_AUTO_WAKE` today.

**Today:** none of `stop.sh`, `session-start.sh`, `precompact.sh`, `pre-*-grep*.sh`, `pre-read-sym.sh`, `pre-whoami-rewrite.sh`, `bullpen-check.sh`, `mark-work.sh`, `no-direct-db.sh`, `no-gh.sh`, `no-local-memory.sh`, `post-task-state.sh`, or any other hook script branches on `LEGION_AUTO_WAKE`.

**Implication:** today's hooks fire identically against watch-spawned and human-attended sessions. The PTY migration does not change that surface area at the hook layer except where this audit explicitly recommends a change (see Site 3).

**Decision:** no behavior change on these hooks. The new `LEGION_SPAWN_SOURCE=watch-pty` marker becomes the audit-required branch point in `stop.sh` (Site 3); the other hooks are left alone.

### Site 3: `plugin/hooks/stop.sh` -- the two gates

`stop.sh` enforces two gates on every Stop event:

1. **Incomplete-work gate (#461)**: scans the session's `TaskList` log and blocks Stop if any task is `pending` or `in_progress`. Today's bypass is `LEGION_SKIP_STOP_BLOCK=1`.
2. **Reflection prompt**: prompts the agent to reflect a finding once per session, gated by a `/tmp/legion-work-${CWD_HASH}` work marker.

Both gates emit `{"decision": "block", "reason": "..."}` -- a CC stop-hook block.

#### Under `LEGION_SPAWN_SOURCE=watch-pty`

**Gate 1 (incomplete-task) decision: SUPPRESS.**

Watch-pty sessions are atomic units of work spawned in response to a specific signal. The wake prompt is short and focused; if work spills past the wake (the agent leaves tasks pending), the right escalation path is:

- The agent posts a `blocked` or `needs_input` signal as the wake's terminal action.
- The next wake (or a human operator) picks it up.

Blocking the Stop on incomplete tasks here risks two failure modes the human-attended path does not have:

- The 8-block stop-hook cap in CC 2.1.143 hard-kills the agent if the gate fires repeatedly. The reaper observes EOF and continues, but the wake's output is truncated.
- Watch's auto-wake is *cooperative*. Suppressing the gate here does not let the agent escape work obligations; it lets the wake exit cleanly so the orchestration layer can decide what to do next.

**Gate 2 (reflect-prompt) decision: SUPPRESS.**

Reflection prompts presume an attended session that just made a discrete change worth noting. Watch wakes are noise-prone:

- Many wakes are responses to signals that resolve in a single tool call; there is no "finding" to capture.
- Even when there is one, the wake's prompt usually surfaces it as a signal already.
- The session-end CLI in `#493` is the right surface for structured wake-time outcomes; the freeform reflection prompt belongs to operator-attended sessions.

Under watch-pty, the per-session reflection prompt is more cost than signal.

#### Mechanism

Add a single early-exit at the top of `stop.sh` immediately after the existing `LEGION_SKIP_STOP_BLOCK` short-circuit:

```bash
# Watch-pty wakes (#492): the two gates below are calibrated for
# operator-attended sessions. Watch-spawned PTY wakes are atomic units
# that exit through this hook on every wake; running the gates risks
# the 8-block stop-hook cap and adds noise without proportionate signal.
# See docs/decisions/2026-05-watch-pty-env-audit.md for the rationale.
if [ "${LEGION_SPAWN_SOURCE:-}" = "watch-pty" ]; then
  if [ -x "$LEGION_BIN" ] && [ -n "$SESSION_ID" ]; then
    "$LEGION_BIN" telemetry record-bypass \
      --repo "$REPO" \
      --session-id "$SESSION_ID" \
      --tool Stop \
      --pattern "watch-pty-skip" \
      --bypass-reason "env:LEGION_SPAWN_SOURCE=watch-pty" \
      2>/dev/null || true
  fi
  exit 0
fi
```

This logs each suppressed gate to `bypass.jsonl` so the operator can audit how often it fires.

The `LEGION_SKIP_STOP_BLOCK=1` bypass is kept as an orthogonal escape for diagnostics / non-watch sessions; it is not subsumed by the watch-pty branch.

## Implementation checklist (for the second commit of #492, gated on #489 landing)

- [ ] Edit `plugin/hooks/stop.sh`: add the early-exit block immediately after the existing `LEGION_SKIP_STOP_BLOCK` bypass.
- [ ] Edit `src/watch.rs` (or wherever `#489` lands the PTY spawn branch): set `LEGION_SPAWN_SOURCE=watch-pty` on the PTY child alongside `LEGION_AUTO_WAKE=1`.
- [ ] Integration test (in `plugin/hooks/test-stop-task-block.sh` or a new test file): feed `LEGION_SPAWN_SOURCE=watch-pty` to `stop.sh` with a pending task in the log; assert exit 0 and a bypass row written.
- [ ] Integration test: feed neither env var; assert the existing block fires unchanged.
- [ ] Docs update: append `LEGION_SPAWN_SOURCE` row to `docs/site/getting-started.md` env-var table.

## Out of scope for #492

- The PTY spawn branch itself (#489).
- Forward-compatibility for `LEGION_AUTO_WAKE=0` -- the env var has never been read by legion code; no callers to migrate.

## Bundled work: #493 stop.sh handoff

Per kessel's bundle doctrine, the `stop.sh` half of #493 (the session-end CLI handoff) lands on the same branch as this audit -- both edit `plugin/hooks/stop.sh`. The CLI subcommand itself (`legion watch session-end --attempt-id <id>`) requires the `mark_wake_attempt_exit_observed` method from #487 and lands in the watch.rs bundle PR alongside #489/#490/#491.

The handoff is forward-compatible: the call is gated on `$LEGION_WAKE_ATTEMPT_ID` being set, the binary call is `|| true`-suppressed, and the CLI subcommand does not exist yet -- the hook is dead code until the consumer lands. The shell-test stub records every CLI invocation so we can assert call shape without a running binary.

Hook firing rules (which paths invoke `session_end_handoff`):
- `LEGION_SKIP_STOP_BLOCK=1` bypass: yes (explicit operator session-end).
- `LEGION_SPAWN_SOURCE=watch-pty` bypass: yes (every watch-pty wake records its exit).
- Reflect-prompt fired previously (marker exists): yes (clean exit path).
- No work marker (session had no real work): yes (clean exit path).
- Incomplete-task gate blocks: no (agent staying alive; would mis-mark exit).
- Reflect-prompt block fires: no (agent staying alive).
