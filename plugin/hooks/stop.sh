#!/bin/bash
# Legion Stop hook.
#
# Two-layer enforcement on every Stop event:
#
# 1. DELEGATED WORK LIVENESS GATE (#778, card-free since #931). A work item
#    delegated to a watch-spawned wake attempt is sound only while an
#    unfakeable liveness signal (watch daemon heartbeat + a linked,
#    in-flight wake_attempts row) backs it. This checks that signal
#    directly against the DB via `legion watch delegated-needs-attention`
#    and blocks when it no longer holds, covering the case
#    `tick_health`'s own auto-revert sweep cannot reach on its own: the
#    watch daemon itself being down.
#
#    In practice this gate is currently VACUOUS: #931 removed the kanban
#    card surface, including `legion kanban delegate` -- the only thing
#    that ever linked a wake_attempts row to a work item. Nothing links one
#    today, so `delegated-needs-attention` always returns empty and this
#    gate never fires. It is kept wired (not deleted) because the
#    underlying liveness predicate (`Database::work_item_is_live`) is
#    legion-only, card-free infrastructure #934 already generalized, and
#    the gate costs nothing to keep sound for whatever eventually
#    re-populates the link. See docs/decisions -- #931's card-surface
#    removal issue -- for why this is a deliberate "vacuous but sound"
#    state, not an oversight.
#
# REMOVED (#931): the in-progress ("Accepted kanban card") gate that used
# to run as gate 1 here (#461 -> #523) had no card-free replacement --
# "an agent has picked-up work in progress" was local card state with no
# work-source equivalent, and inventing a local claim ledger to preserve it
# was explicitly out of scope for the card removal (see `src/queue.rs`'s
# module doc comment: no local "accepted" claim survives). Removing the
# stale `legion kanban list` call rather than leaving it in place matters:
# a dead subcommand call fails open silently and LOOKS like a working
# gate, which is worse than an admittedly-absent one. This is reported as
# a blocker in #931's work summary, not silently dropped.
#
# 2. REFLECTION PROMPT. If work happened this session and the reflection
#    hasn't fired yet, nudge for one via hookSpecificOutput.additionalContext
#    (#569) -- non-error feedback that continues the turn so the agent acts on
#    it, WITHOUT the hook-error labeling and 8-block cap that decision:block
#    incurs. One-shot per session ($MARKER) + stop_hook_active guarded. Skip
#    when nothing was learned.
#
# Bypass: LEGION_SKIP_STOP_BLOCK=1 env skips both gates. Writes a
# telemetry row via `legion telemetry record-bypass` so the escape is
# visible to #440's summary.

# shellcheck source=lib/prelude.sh
source "${CLAUDE_PLUGIN_ROOT:-}/hooks/lib/prelude.sh" 2>/dev/null || exit 0
# shellcheck source=lib/emit.sh
source "${CLAUDE_PLUGIN_ROOT:-}/hooks/lib/emit.sh" 2>/dev/null || exit 0

legion_hook_parse || exit 0
# stop_hook_active is true when this Stop was itself triggered by a prior hook
# continuation. The reflection nudge (which now continues the turn via
# additionalContext, #569) honors it as a loop guard so a continuation we
# caused does not re-nudge. The delegated-liveness block deliberately ignores
# it -- that gate must keep firing while a dead delegation exists (the
# 8-block cap is its own backstop).
STOP_ACTIVE=$(echo "$INPUT" | jq -r '.stop_hook_active // false' 2>/dev/null)

if [ -z "$CWD" ]; then
  exit 0
fi

CWD_HASH=$(legion_hash_str "$CWD")

# Session-end handoff (#493): optimistic expediter for the watch
# reaper. Lets the reaper skip a poll cycle when the agent has cleanly
# exited and CC fires this hook. PTY EOF + PID-poll remain the
# authoritative completion signal; this is a speed-up only, so the
# `|| true` swallows any failure -- the reaper still converges via EOF
# even if the CLI errors or does not exist yet (forward-compatible).
#
# The CLI subcommand `legion watch session-end --attempt-id <id>` lands
# in the watch.rs bundle PR alongside #489/#490/#491. Until then this
# call is a no-op that proves out the wire-up. Function must be defined
# before any caller; placed up here so all subsequent bypass paths can
# invoke it cleanly.
session_end_handoff() {
  if [ -n "${LEGION_WAKE_ATTEMPT_ID:-}" ] && [ -x "$LEGION" ]; then
    "$LEGION" watch session-end \
      --attempt-id "$LEGION_WAKE_ATTEMPT_ID" \
      >/dev/null 2>&1 || true
  fi
}

# Bypass: skip both gates, log the escape if telemetry is available.
if [ "${LEGION_SKIP_STOP_BLOCK:-}" = "1" ]; then
  if [ -x "$LEGION" ] && [ -n "$SESSION_ID" ]; then
    "$LEGION" telemetry record-bypass \
      --repo "$REPO" \
      --session-id "$SESSION_ID" \
      --tool Stop \
      --pattern "session-end" \
      --bypass-reason "env:LEGION_SKIP_STOP_BLOCK=1" \
      2>/dev/null || true
  fi
  # Explicit operator session-end: still fire the #493 handoff so a
  # watch-spawned session that exits via this bypass records its
  # exit_observed_at timestamp for the reaper.
  session_end_handoff
  exit 0
fi

# Watch-pty wakes (#492): the two gates below are calibrated for
# operator-attended sessions. Watch-spawned PTY wakes are atomic units
# that exit through this hook on every wake; running the gates risks
# the 8-block stop-hook cap in CC 2.1.143 and adds noise without
# proportionate signal. Reaper observes EOF and continues regardless.
# Rationale + per-gate decisions in docs/decisions/2026-05-watch-pty-env-audit.md.
#
# LEGION_SPAWN_SOURCE=watch-pty is stamped on every PTY spawn by
# src/watch/spawn.rs's SpawnMode::Pty branch (#489, live at HEAD) -- this
# is not a forward-compat stub, it fires on every real watch-spawned wake.
if [ "${LEGION_SPAWN_SOURCE:-}" = "watch-pty" ]; then
  if [ -x "$LEGION" ] && [ -n "$SESSION_ID" ]; then
    "$LEGION" telemetry record-bypass \
      --repo "$REPO" \
      --session-id "$SESSION_ID" \
      --tool Stop \
      --pattern "watch-pty-skip" \
      --bypass-reason "env:LEGION_SPAWN_SOURCE=watch-pty" \
      2>/dev/null || true
  fi
  session_end_handoff
  exit 0
fi

# ---------- (1) Delegated work liveness gate (#778, card-free since #931) ----------
#
# Fail-open at the shell-call layer only (a broken legion binary or missing
# jq must never trap the agent). The liveness ANSWER itself is fail-closed
# inside the subcommand: a missing link, a terminal attempt, or a
# stale/absent daemon heartbeat all read as needs-attention, never as "safe
# to stop" -- an ambiguous state must never be interpreted as safe.
if command -v jq >/dev/null 2>&1 && [ -x "$LEGION" ]; then
  DEAD_DELEGATED=$("$LEGION" watch delegated-needs-attention --repo "$REPO" --json 2>/dev/null \
    | jq -r '"- " + .work_item_id' 2>/dev/null)

  if [ -n "$DEAD_DELEGATED" ]; then
    REASON="Work delegated to a watch-spawned attempt is no longer verifiably live (the attempt finished or died, or the watch daemon itself is not running) and cannot stop yet:

${DEAD_DELEGATED}

This should self-heal within one watch health tick once the daemon is running again; if it persists, check \`legion watch status\`.

To bypass (rare, diagnostics or explicit operator session-end), set LEGION_SKIP_STOP_BLOCK=1. The bypass writes one row to bypass.jsonl."

    emit_block "$REASON"
    exit 0
  fi
fi

# ---------- (2) Reflection prompt ----------
#
# Prevent re-fires: one reflect prompt per session
MARKER="/tmp/legion-reflected-${CWD_HASH}"
if [ -f "$MARKER" ]; then
  # Agent is exiting (no block fired); fire the #493 handoff so the
  # reaper can skip a poll cycle.
  session_end_handoff
  exit 0
fi

# Skip if session had no real work
WORK_MARKER="/tmp/legion-work-${CWD_HASH}"
if [ ! -f "$WORK_MARKER" ]; then
  # Same as above -- clean exit path, fire the handoff.
  session_end_handoff
  exit 0
fi

# Loop guard: if this Stop is itself a hook-induced continuation, the nudge
# already fired -- do not re-emit. The on-disk $MARKER is the primary guard
# (the next Stop hits the marker check above and exits 0); this is the
# belt-and-suspenders for additionalContext's continue-the-turn behavior.
if [ "$STOP_ACTIVE" = "true" ]; then
  session_end_handoff
  exit 0
fi

touch "$MARKER"

REASON="Drop one thing a teammate would not have known walking in cold -- a gotcha, a hidden invariant, how something actually works. Not what you did; the finding itself. Store it: legion reflect --repo $REPO --text '<finding>'. Skip if nothing surprising came up."

# Budget reminder (#524) -- the "on stop" half of surfacing the autonomy
# budget. Tells the agent, as it wraps up, that it has sanctioned units left
# to self-direct more work, so stopping is a choice, not a default. Fail-open:
# any error leaves BUDGET empty and the reflection prompt fires unchanged.
BUDGET=$("$LEGION" autonomy status --repo "$REPO" --banner 2>/dev/null)
if [ -n "$BUDGET" ]; then
  REASON="${REASON}

${BUDGET}"
fi

# #569: the reflection nudge continues the turn via additionalContext, NOT
# decision:block. Verified against CC 2.1.168: Stop additionalContext is
# "non-error feedback that continues the conversation" -- the agent receives
# the nudge and acts on it, but without the hook-error labeling and the
# 8-consecutive-block cap that decision:block incurs. The delegated-liveness
# gate above stays a hard decision:block on purpose (it must be able to
# refuse the stop, and wants the cap as a safety valve); this softer nudge is
# the right tool for a once-per-session prompt the agent should act on.
emit_context "Stop" "$REASON"
