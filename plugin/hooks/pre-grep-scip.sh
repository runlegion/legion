#!/bin/bash
# Legion PreToolUse hook for Grep/Glob: when the pattern looks like a bare
# symbol, query `legion sym def` + `legion sym refs` and inject results
# as additionalContext so the agent gets file:line answers instead of
# burning tokens on a full-tree grep (#283).
#
# Symbol-intent heuristic: pattern qualifies if every check passes.
#   - length > 2 after trimming whitespace
#   - only word chars (a-z A-Z 0-9 _) with optional :: or . separators
#   - no regex metacharacters
#
# Suppression: LEGION_SKIP_PRE_SCIP=1 short-circuits.
# Override result count via LEGION_SCIP_INJECT_LIMIT (default 5; can also
# be sourced from plugin/hooks/scip-clamp.conf).

LEGION="${CLAUDE_PLUGIN_ROOT}/bin/legion"
LOG=/tmp/legion-hook-errors.log

if [ "${LEGION_SKIP_PRE_SCIP:-}" = "1" ]; then
  exit 0
fi

INPUT=$(cat)
if [ -z "$INPUT" ]; then
  exit 0
fi

CWD=$(printf '%s' "$INPUT" | jq -r '.cwd // empty' 2>/dev/null)
TOOL=$(printf '%s' "$INPUT" | jq -r '.tool_name // empty' 2>/dev/null)
PATTERN=$(printf '%s' "$INPUT" | jq -r '.tool_input.pattern // empty' 2>/dev/null)
SESSION_ID=$(printf '%s' "$INPUT" | jq -r '.session_id // empty' 2>/dev/null)

if [ -z "$CWD" ] || [ -z "$TOOL" ] || [ -z "$PATTERN" ]; then
  exit 0
fi

REPO="${LEGION_REPO:-$(basename "$CWD")}"

# Coverage gate (#353): no-op silently in uncovered repos.
if [ -f "${CLAUDE_PLUGIN_ROOT}/hooks/_legion-covered.sh" ]; then
  # shellcheck source=_legion-covered.sh
  source "${CLAUDE_PLUGIN_ROOT}/hooks/_legion-covered.sh"
  if ! legion_covered "$SESSION_ID" "$REPO"; then
    exit 0
  fi
fi

# Trim whitespace.
TRIMMED=$(printf '%s' "$PATTERN" | tr -d '[:space:]')

# Length check.
if [ "${#TRIMMED}" -le 2 ]; then
  exit 0
fi

# Reject if any regex metacharacter is present. Backslashes count too.
case "$TRIMMED" in
  *[\.\*\+\?\[\]\(\)\{\}\^\$\|\\]*)
    exit 0
    ;;
esac

# Allow only word chars with optional :: or . separators. We already
# rejected `.` above, so the only legal separators here are `::`.
# Symbol pattern: ^[A-Za-z_][A-Za-z0-9_]*((::[A-Za-z_][A-Za-z0-9_]*)+)?$
if ! [[ "$TRIMMED" =~ ^[A-Za-z_][A-Za-z0-9_]*(::[A-Za-z_][A-Za-z0-9_]*)*$ ]]; then
  exit 0
fi

# Glob extra constraint: must have no wildcard.
if [ "$TOOL" = "Glob" ]; then
  case "$PATTERN" in
    *\**|*\?*|*\[*|*\{*)
      exit 0
      ;;
  esac
fi

if [ ! -x "$LEGION" ]; then
  echo "[pre-grep-scip] legion binary not found at ${LEGION}; skipping" >&2
  exit 0
fi

# Optional clamp config.
CLAMP_FILE="${CLAUDE_PLUGIN_ROOT}/hooks/scip-clamp.conf"
if [ -f "$CLAMP_FILE" ]; then
  # shellcheck source=/dev/null
  source "$CLAMP_FILE"
fi
LIMIT="${LEGION_SCIP_INJECT_LIMIT:-5}"

run_with_timeout() {
  local action="$1"
  if command -v gtimeout >/dev/null 2>&1; then
    gtimeout 2 "$LEGION" sym "$action" "$TRIMMED" --repo "$REPO" 2>>"$LOG"
  else
    perl -e 'alarm 2; exec @ARGV' "$LEGION" sym "$action" "$TRIMMED" --repo "$REPO" 2>>"$LOG"
  fi
}

DEFS=$(run_with_timeout def)
DEFS_RC=$?
REFS=$(run_with_timeout refs)
REFS_RC=$?

# Treat timeout (124 / 142) as soft-fail: exit 0 with no injection.
if [ "$DEFS_RC" -eq 124 ] || [ "$DEFS_RC" -eq 142 ] \
   || [ "$REFS_RC" -eq 124 ] || [ "$REFS_RC" -eq 142 ]; then
  echo "[pre-grep-scip] sym query timed out for ${TRIMMED}" >&2
  exit 0
fi

# Both errored hard: skip.
if [ "$DEFS_RC" -ne 0 ] && [ "$REFS_RC" -ne 0 ]; then
  echo "[pre-grep-scip] sym query failed (def=${DEFS_RC} refs=${REFS_RC}); skipping" >&2
  exit 0
fi

# Trim each set to LIMIT lines.
DEFS_TRIM=$(printf '%s' "$DEFS" | head -n "$LIMIT")
REFS_TRIM=$(printf '%s' "$REFS" | head -n "$LIMIT")

if [ -z "$DEFS_TRIM" ] && [ -z "$REFS_TRIM" ]; then
  exit 0
fi

CTX="## SCIP symbol results for \`${TRIMMED}\`

Definitions:
${DEFS_TRIM:-(none)}

References:
${REFS_TRIM:-(none)}

If these answer your question, skip the ${TOOL}. Otherwise the original ${TOOL} will run as you asked."

jq -n --arg ctx "$CTX" '{
  "hookSpecificOutput": {
    "hookEventName": "PreToolUse",
    "permissionDecision": "allow",
    "permissionDecisionReason": "legion sym hits for the symbol",
    "additionalContext": $ctx
  }
}'
