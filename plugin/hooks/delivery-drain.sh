#!/bin/bash
# Legion hook-side delivery drain (#941): the live-session delivery lane.
# Runs `legion deliver drain --repo "$REPO" --split` and injects any
# drained bullpen posts/signals as a RESULT (not a note -- see the HOOK
# OUTPUT DOCTRINE below). It ran alongside the MCP subprocess's
# `notifications/claude/channel` push through a dual-lane parity window
# and became the sole live-session lane when that push was retired
# (#947); unlike the push, it needs no model inference roundtrip to fire.
#
# HOOK OUTPUT DOCTRINE (Sean, 2026-08-27, reflection 01a0421f-3bb1-7a91-
# a2d4-1587442dbd36, after the missed rfc 01a04213 -- #1020): a wake-worthy
# signal sat unanswered for forty minutes because the drain injected it as
# one `[Legion] Delivered via hook drain:` note mid-stream inside a tool
# result, indistinguishable from ordinary musings. Two things fixed that:
#
#   1. hooks.json wires this hook as the LAST group for every event it
#      runs on (UserPromptSubmit, PostToolUse, Stop), so its block is the
#      last thing the model reads for that event.
#   2. This hook's own output is a RESULT block with a fixed opening and
#      closing line ($OPEN_LINE / $CLOSE_LINE below), never bare prose.
#      `--split` has `legion deliver drain` do the sorting: musings (the
#      existing lighter `[Legion] Bullpen (...)` header) come first,
#      directed wake-worthy signals -- in the `legion pending-replies`
#      REQUIRES A REPLY shape, via the shared `board::format_pending_
#      replies` formatter -- come LAST inside the block. Nothing follows
#      the closing line.
#
# Wired into UserPromptSubmit, PostToolUse (alongside mark-work.sh), and
# Stop -- three points in a turn where surfacing a mid-session post is
# cheap. PostToolUse fires on every tool call, so this debounces via a
# per-session last-drain-time sentinel under
# ${XDG_CACHE_HOME:-$HOME/.cache}/legion, modeled on
# identity-chain-load.sh's sentinel pattern -- except keyed by last-drain
# TIME rather than a one-shot flag, so the hook re-arms after the debounce
# window instead of firing exactly once per session.
#
# Stop is EXEMPT from the debounce (#1000). Stop fires once per turn and is
# the last chance to drain before a session goes idle, where no further hook
# fires until the agent acts again. Because Stop follows the last tool call,
# it almost always lands inside the window a prior PostToolUse just opened --
# so debouncing it swallowed the turn-end drain in the common case, leaving a
# post that arrived late in the turn unseen. The debounce still governs
# UserPromptSubmit and PostToolUse, whose whole risk is per-tool-call
# shell-out volume; Stop has neither that volume nor a later drain to fall
# back on. A Stop drain that finds nothing new emits nothing and the turn
# ends normally, so the exemption cannot loop. Stop's own directed-signal
# obligation is additionally hard-enforced by stop.sh's pending-replies
# gate (#1020), which blocks the turn outright rather than relying on this
# hook's softer additionalContext.
#
# Error handling: legion failures are logged to /tmp/legion-hook-errors.log.
# The hook always exits 0 so a degraded legion never blocks the turn.

# shellcheck source=lib/prelude.sh
source "${CLAUDE_PLUGIN_ROOT:-}/hooks/lib/prelude.sh" 2>/dev/null || exit 0
# shellcheck source=lib/emit.sh
source "${CLAUDE_PLUGIN_ROOT:-}/hooks/lib/emit.sh" 2>/dev/null || exit 0

LOG="$LEGION_HOOK_LOG"

legion_hook_parse || exit 0

if [ -z "$REPO" ] || [ -z "$SESSION_ID" ]; then
  exit 0
fi

if [ ! -x "$LEGION" ]; then
  exit 0
fi

# Skip in repos legion does not cover (#353) -- PostToolUse fires on every
# tool call, so an uncovered repo should not pay a `legion deliver drain`
# shell-out on each one.
legion_hook_covered || exit 0

# The event that fired this invocation (PostToolUse/UserPromptSubmit/Stop
# all wire this same script). Every hook payload carries hook_event_name;
# fall back to UserPromptSubmit defensively if it is ever missing.
EVENT_NAME=$(legion_hook_field '.hook_event_name')
[ -n "$EVENT_NAME" ] || EVENT_NAME="UserPromptSubmit"

# Minimum seconds between drains for one session. PostToolUse can fire many
# times a minute; this keeps the drain from shelling out to `legion` on
# every single tool call.
DEBOUNCE_SECONDS="${LEGION_DELIVERY_DRAIN_DEBOUNCE_SECONDS:-10}"

SAFE_SESSION=$(printf '%s' "$SESSION_ID" | tr -c 'a-zA-Z0-9_-' '_')

# A failed mkdir is logged, not fatal: the drain still runs, but with no
# sentinel the debounce never engages and every tool call pays the full
# shell-out -- the log line is the only symptom, so keep it.
CACHE_DIR="${XDG_CACHE_HOME:-$HOME/.cache}/legion"
mkdir -p "$CACHE_DIR" 2>>"$LOG"
SENTINEL="${CACHE_DIR}/delivery-drain-last-${SAFE_SESSION}"

NOW=$(date +%s)
# Stop is never debounced (see header): it fires once per turn and is the
# last drain before idle. Only UserPromptSubmit/PostToolUse consult the
# window. Stop still updates the sentinel below, so a drain it performs
# debounces the next tool-call flurry as usual.
if [ "$EVENT_NAME" != "Stop" ] && [ -f "$SENTINEL" ]; then
  LAST=$(cat "$SENTINEL" 2>/dev/null)
  case "$LAST" in
    '' | *[!0-9]*) LAST=0 ;;
  esac
  ELAPSED=$((NOW - LAST))
  if [ "$ELAPSED" -lt "$DEBOUNCE_SECONDS" ]; then
    exit 0
  fi
fi

printf '%s' "$NOW" >"$SENTINEL" 2>/dev/null

DRAINED=$("$LEGION" deliver drain --repo "$REPO" --split 2>>"$LOG")

if [ -z "$DRAINED" ]; then
  exit 0
fi

# Fixed opening and closing lines (#1020): this is a RESULT, not a note --
# see the HOOK OUTPUT DOCTRINE above. --split already ordered $DRAINED as
# musings, then a separator, then the directed REQUIRES A REPLY set;
# nothing follows the closing line.
OPEN_LINE="[Legion] Delivery drain result:"
CLOSE_LINE="[Legion] End delivery drain result."

CTX="${OPEN_LINE}
${DRAINED}

${CLOSE_LINE}"

emit_context "$EVENT_NAME" "$CTX" 2>>"$LOG" || true

exit 0
