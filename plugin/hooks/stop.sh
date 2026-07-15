#!/bin/bash
# Legion Stop hook.
#
# Three-layer enforcement on every Stop event:
#
# 1. IN-PROGRESS GATE (#461 -> #523). If the agent has an Accepted
#    (in-progress) kanban card for this repo, block the Stop until it
#    reaches a real state, via decision:block (a HARD refusal -- the agent
#    cannot stop with picked-up work). Reads the BOARD -- the persistent work
#    substrate that `work`/`accept` actually move -- not the ephemeral
#    per-session harness TaskList. Gates on Accepted, NOT Pending: Pending is
#    the unconsented backlog, which would never let the agent stop; a declared
#    blocker moves a card to Blocked, excluded here by construction. No silent
#    abandonment of picked-up work. The board-derived goal (#525) rides along.
#
# 1b. DELEGATED WORK LIVENESS GATE (#778). A card in `delegated` is invisible
#    to gate 1's Accepted-only select by construction -- sound only while an
#    unfakeable liveness signal (watch daemon heartbeat + a linked, in-flight
#    wake_attempts row) backs it. This re-checks that signal directly against
#    the DB and blocks the same way when it no longer holds, covering the one
#    case the auto-revert sweep cannot reach on its own: the watch daemon
#    itself being down.
#
# 2. REFLECTION PROMPT. If work happened this session and the reflection
#    hasn't fired yet, nudge for one via hookSpecificOutput.additionalContext
#    (#569) -- non-error feedback that continues the turn so the agent acts on
#    it, WITHOUT the hook-error labeling and 8-block cap that decision:block
#    incurs. One-shot per session ($MARKER) + stop_hook_active guarded. Skip
#    when nothing was learned.
#
# Bypass: LEGION_SKIP_STOP_BLOCK=1 env skips all three gates. Writes a
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
# caused does not re-nudge. The in-progress block deliberately ignores it --
# that gate must keep firing while Accepted work exists (the 8-block cap is its
# own backstop).
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

# ---------- (1) In-progress work gate ----------
#
# Block the stop if the agent has an Accepted (in-progress) kanban card for
# this repo. Reads the board via `kanban list --json` (the persistent work
# substrate), NOT the ephemeral per-session TaskList log. Gate on Accepted
# only: Pending is the unconsented backlog (gating on it would never let the
# agent stop), and a declared blocker moves a card to Blocked, excluded here.
#
# Repo-scoped, not session-scoped: this blocks on ANY Accepted card for $REPO,
# not just this session's. Correct under legion's one-agent-per-repo board
# model; revisit if multiple agents ever share a repo board.
#
# Fail-open by design: the legion call and jq both redirect 2>/dev/null, so if
# the binary errors (DB lock, migration, panic) or jq is missing, ACCEPTED is
# empty and the Stop is ALLOWED. A hook that governs stopping must never trap
# the agent on an infra hiccup -- LEGION_SKIP_STOP_BLOCK is the deliberate
# escape, but failing open is the safe direction when the check itself breaks.

if command -v jq >/dev/null 2>&1 && [ -x "$LEGION" ]; then
  ACCEPTED=$("$LEGION" kanban list --repo "$REPO" --json 2>/dev/null \
    | jq -r 'select(.status == "accepted") | "- " + (.title // .text // .id)' 2>/dev/null)

  if [ -n "$ACCEPTED" ]; then
    REASON="You have in-progress (Accepted) work on the board and cannot stop yet:

${ACCEPTED}

Move each card to a real state before stopping:
- finished: let its PR merge / mark it done
- technical blocker: legion kanban block --id <id> --reason '...'
- needs a human decision: legion kanban need-input --id <id>
- not yours to do now: hand it back to pending

The board is the plan, and the plan is the permission -- \"should I keep going?\" is not a question to ask the operator after every step. Keep going until the card is in a real state.

To bypass (rare, diagnostics or explicit operator session-end), set LEGION_SKIP_STOP_BLOCK=1. The bypass writes one row to bypass.jsonl."

    # Surface the board-derived goal (#525) so the completion condition the
    # agent must drive the Accepted card to is in front of it, not just "keep
    # going." Fail-open: any error leaves GOAL empty and the block fires plain.
    GOAL=$("$LEGION" goal --repo "$REPO" 2>/dev/null)
    if [ -n "$GOAL" ]; then
      REASON="${REASON}

${GOAL}"
    fi

    emit_block "$REASON"
    exit 0
  fi
fi

# ---------- (1b) Delegated work liveness gate (#778) ----------
#
# A card in `delegated` is invisible to the Accepted gate above BY
# CONSTRUCTION (it selects `.status == "accepted"` only). That is safe ONLY
# while the delegation is verifiably live: the watch daemon's heartbeat is
# fresh AND the card's linked wake_attempts row is in an in-flight state --
# the exact predicate `legion kanban delegate` required to enter the state,
# and the one `tick_health`'s auto-revert sweep uses to leave it (one shared
# predicate, `Database::delegated_card_is_live`, not one per call site that
# could silently disagree -- the #679 lesson). `legion kanban
# delegated-needs-attention` re-runs that predicate directly against the DB
# and lists any delegated card that fails it.
#
# tick_health normally reverts a dead delegation back to Accepted within one
# health tick, which would make the gate above fire on its own. This check
# covers the gap that leaves open: the watch daemon itself may be the thing
# that died, in which case nothing is left running to perform that revert --
# this hook is the only remaining backstop, so it re-checks liveness itself
# rather than trusting the card's current status.
#
# Fail-open matches gate (1) above at the shell-call layer only (a broken
# legion binary or missing jq must never trap the agent). The liveness
# ANSWER itself is fail-closed inside the subcommand: a missing link, a
# terminal attempt, or a stale/absent daemon heartbeat all read as
# needs-attention, never as "safe to stop" -- an ambiguous state must never
# be interpreted as safe.
if command -v jq >/dev/null 2>&1 && [ -x "$LEGION" ]; then
  DEAD_DELEGATED=$("$LEGION" kanban delegated-needs-attention --repo "$REPO" --json 2>/dev/null \
    | jq -r '"- " + (.title // .text // .id)' 2>/dev/null)

  if [ -n "$DEAD_DELEGATED" ]; then
    REASON="You delegated work on the board to a watch-spawned attempt, but it is no longer verifiably live (the attempt finished or died, or the watch daemon itself is not running) and cannot stop yet:

${DEAD_DELEGATED}

Move each card to a real state before stopping:
- resume it yourself: legion kanban undelegate --id <id>
- re-delegate to a fresh live attempt: legion kanban delegate --id <id> --attempt-id <attempt>
- technical blocker: legion kanban block --id <id> --reason '...'
- needs a human decision: legion kanban need-input --id <id>

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

# Surface the board-derived goal (#525) with the nudge so the completion
# condition carries across the turn. Fail-open: empty GOAL leaves it off.
GOAL=$("$LEGION" goal --repo "$REPO" 2>/dev/null)
if [ -n "$GOAL" ]; then
  REASON="${REASON}

${GOAL}"
fi

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
# 8-consecutive-block cap that decision:block incurs. The in-progress gate
# above stays a hard decision:block on purpose (it must be able to refuse the
# stop, and wants the cap as a safety valve); this softer nudge is the right
# tool for a once-per-session prompt the agent should act on.
emit_context "Stop" "$REASON"
