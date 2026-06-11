#!/bin/bash
# Legion UserPromptSubmit hook: lazy-load the identity chain on the first
# user prompt of a session. Pairs with the 2KB SessionStart whoami banner
# (#342) -- the banner stays slim, the chain pays for itself once when the
# agent actually starts working.
#
# Idempotency: per-session sentinel at /tmp/legion-chain-loaded-${SESSION_ID}.
# First prompt walks the chain and injects it; subsequent prompts short-circuit
# and exit silently.
#
# Error handling: legion failures are logged to /tmp/legion-hook-errors.log.
# The hook always exits 0 so a degraded legion never blocks user prompts.

LEGION="${CLAUDE_PLUGIN_ROOT}/bin/legion"
LOG=/tmp/legion-hook-errors.log

INPUT=$(cat)
CWD=$(echo "$INPUT" | jq -r '.cwd // empty')
SESSION_ID=$(echo "$INPUT" | jq -r '.session_id // empty')

if [ -z "$CWD" ] || [ -z "$SESSION_ID" ]; then
  exit 0
fi

# Sanitize session id for use as a filename component. Use printf, not echo,
# to avoid the trailing newline being mapped to '_' by tr.
SAFE_SESSION=$(printf '%s' "$SESSION_ID" | tr -c 'a-zA-Z0-9_-' '_')

# Sentinel lives under a user-scoped cache dir, not /tmp, so a shared host
# cannot pre-create the path (or a symlink to a sensitive file) to either
# suppress chain load or redirect the touch.
CACHE_DIR="${XDG_CACHE_HOME:-$HOME/.cache}/legion"
mkdir -p "$CACHE_DIR" 2>/dev/null
SENTINEL="${CACHE_DIR}/chain-loaded-${SAFE_SESSION}"

# Already loaded for this session -- nothing to do.
if [ -f "$SENTINEL" ]; then
  exit 0
fi

REPO=$(basename "$CWD")

# Find the newest identity reflection. legion chain --id walks both backward
# and forward from any node, so any identity reflection ID gets the whole
# chain. The newest reflection is most likely to be the active root.
RECALL_OUT=$("$LEGION" recall --repo "$REPO" --domain identity --limit 1 --preview 60 2>>"$LOG")
ROOT_LINE=$(printf '%s\n' "$RECALL_OUT" | grep -oE '\(id: [0-9a-f-]+' | head -1)

if [ -z "$ROOT_LINE" ]; then
  # No identity reflections -- nothing to chain. Touch sentinel so we don't
  # retry on every prompt this session.
  touch "$SENTINEL"
  exit 0
fi

ROOT_ID="${ROOT_LINE#(id: }"

CHAIN=$("$LEGION" chain --id "$ROOT_ID" --full 2>>"$LOG")

# Mark loaded regardless of chain content -- a single-node chain (no children)
# returns just the root, which the agent already saw via whoami. No need to
# inject again.
touch "$SENTINEL"

# Single-reflection chains contain only the root, which is already in the
# SessionStart banner. Skip injection in that case to avoid redundancy.
LINK_COUNT=$(echo "$CHAIN" | grep -c '^--- ')
if [ "$LINK_COUNT" -lt 2 ]; then
  exit 0
fi

CTX="[Legion] Identity chain (deeper doctrine, walked once per session):

${CHAIN}
This chain extends the identity banner you saw at session start. The root reflection appears first; subsequent links carry doctrine in decreasing importance order. Internalize the rules, do not just acknowledge them."

jq -n --arg ctx "$CTX" '{
  "hookSpecificOutput": {
    "hookEventName": "UserPromptSubmit",
    "additionalContext": $ctx
  }
}' 2>>"$LOG" || true

# Always exit 0 so a degraded legion (or jq failure) never blocks user
# prompts. The script either injected context or it didn't; either way the
# user's prompt should still flow.
exit 0
