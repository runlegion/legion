#!/bin/bash
# Legion PreToolUse hook (#283): if a Grep or Glob pattern looks like a
# bare symbol (CamelCase or snake_case, no regex noise, length > 2), run
# `legion sym def --json <symbol>` and inject results as additionalContext
# before the search tool fires.
#
# Sibling of pre-grep-recall.sh. Same skip discipline, same JSON I/O shape,
# same coverage gate. Never blocks the tool. Falls through silently if
# legion has no SCIP index for the repo or the pattern is not a symbol.
#
# Differences from pre-grep-recall.sh:
# - Uses `legion sym def --json` (not `legion recall`)
# - Pattern detection is symbol-shape, not natural-language tokenization
# - LEGION_SKIP_PRE_GREP_SCIP=1 skips this hook specifically
#
# Error handling: every failure exits 0 with a stderr breadcrumb prefixed
# "[legion-pre-grep-scip]". Errors append to /tmp/legion-hook-errors.log.

LEGION="${CLAUDE_PLUGIN_ROOT}/bin/legion"
LOG=/tmp/legion-hook-errors.log

if [ "${LEGION_SKIP_PRE_GREP_SCIP:-}" = "1" ]; then
  echo "[legion-pre-grep-scip] skipped (LEGION_SKIP_PRE_GREP_SCIP=1)" >&2
  exit 0
fi

INPUT=$(cat)
if [ -z "$INPUT" ]; then
  exit 0
fi

CWD=$(echo "$INPUT" | jq -r '.cwd // empty' 2>/dev/null)
TOOL=$(echo "$INPUT" | jq -r '.tool_name // empty' 2>/dev/null)
PATTERN=$(echo "$INPUT" | jq -r '.tool_input.pattern // empty' 2>/dev/null)

if [ -z "$CWD" ] || [ -z "$TOOL" ] || [ -z "$PATTERN" ]; then
  exit 0
fi

# Only apply to Grep and Glob.
case "$TOOL" in
  Grep|Glob) ;;
  *) exit 0 ;;
esac

REPO="${LEGION_REPO:-$(basename "$CWD")}"
if [ -z "$REPO" ]; then
  exit 0
fi

# Skip uncovered repos.
SESSION_ID=$(echo "$INPUT" | jq -r '.session_id // empty' 2>/dev/null)
if [ -f "${CLAUDE_PLUGIN_ROOT}/hooks/_legion-covered.sh" ]; then
  # shellcheck source=_legion-covered.sh
  source "${CLAUDE_PLUGIN_ROOT}/hooks/_legion-covered.sh"
  if ! legion_covered "$SESSION_ID" "$REPO"; then
    exit 0
  fi
fi

# Symbol-shape detection: CamelCase or snake_case identifier, length > 2,
# no regex metacharacters. This is the conservative case where the agent
# is almost certainly looking for a definition or reference, not pattern
# matching. Strip leading and trailing word boundaries (\b) if present
# (a common Grep convention).
SYMBOL=$(echo "$PATTERN" | sed -E 's/^\\b//; s/\\b$//')

# Symbol regex covers both CamelCase and snake_case identifiers, length > 2,
# and excludes regex metacharacters and whitespace by construction. If the
# whole string does not match, this is a regex pattern, not a bare symbol.
if ! echo "$SYMBOL" | grep -qE '^[A-Z][A-Za-z0-9_]{2,}$|^[a-z_][a-z_0-9]{2,}$'; then
  exit 0
fi

# Bail if the legion binary is missing.
if [ ! -x "$LEGION" ]; then
  exit 0
fi

# Run sym def. The hook-level timeout in hooks.json (6s) bounds runtime;
# no separate gtimeout wrapper here.
HITS=$("$LEGION" sym def --json "$SYMBOL" 2>>"$LOG")
RC=$?

# Empty results or error -> skip injection.
if [ "$RC" -ne 0 ] || [ -z "$HITS" ] || [ "$HITS" = "[]" ] || [ "$HITS" = "null" ]; then
  exit 0
fi

# Also fetch refs for context (best-effort; skip on failure).
REFS=$("$LEGION" sym refs --json "$SYMBOL" 2>>"$LOG" || true)
REFS_LINE=""
if [ -n "$REFS" ] && [ "$REFS" != "[]" ] && [ "$REFS" != "null" ]; then
  REF_COUNT=$(echo "$REFS" | jq 'length' 2>/dev/null || echo "0")
  REFS_LINE="(${REF_COUNT} reference(s) found via \`legion sym refs ${SYMBOL}\`)"
fi

CTX="## Legion SCIP for \`${SYMBOL}\`

Before ${TOOL} on \`${PATTERN}\`, legion's pre-computed SCIP index returned:

\`\`\`json
${HITS}
\`\`\`

${REFS_LINE}

If this answers your symbol question, skip the ${TOOL}. SCIP is byte-cheap; file scans are not."

jq -n --arg ctx "$CTX" '{
  "hookSpecificOutput": {
    "hookEventName": "PreToolUse",
    "permissionDecision": "allow",
    "permissionDecisionReason": "legion sym hits for the symbol",
    "additionalContext": $ctx
  }
}'
