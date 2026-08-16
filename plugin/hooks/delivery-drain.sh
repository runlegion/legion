#!/bin/bash
# Legion hook-side delivery drain (#941): the code-side counterpart to the
# MCP notification lane's push. Runs `legion deliver drain --repo "$REPO"`
# and injects any drained bullpen posts/signals as additionalContext -- the
# same delivery an interactive session would otherwise only get through the
# MCP subprocess's `notifications/claude/channel` push, which depends on a
# model inference roundtrip existing at all.
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

CACHE_DIR="${XDG_CACHE_HOME:-$HOME/.cache}/legion"
mkdir -p "$CACHE_DIR" 2>/dev/null
SENTINEL="${CACHE_DIR}/delivery-drain-last-${SAFE_SESSION}"

NOW=$(date +%s)
if [ -f "$SENTINEL" ]; then
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

DRAINED=$("$LEGION" deliver drain --repo "$REPO" 2>>"$LOG")

if [ -z "$DRAINED" ]; then
  exit 0
fi

CTX="[Legion] Delivered via hook drain (dual-lane parity with the MCP channel push):

${DRAINED}"

emit_context "$EVENT_NAME" "$CTX" 2>>"$LOG" || true

exit 0
