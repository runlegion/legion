#!/bin/bash
# Legion PreToolUse hook (#439): when Read targets a large source file in
# an indexed repo without a `limit`, block with a nudge toward
# `legion sym hover` or a bounded Read.
#
# Whole-file Read on src/main.rs (~6K lines) bills cache_read for every
# line; 99% of the time the agent only needs one symbol's signature +
# docstring, which `legion sym hover Symbol` answers in bytes.
#
# Three-state ladder (consistent with #438):
#   1. INJECT  -- not implemented in v1 (would require a new `legion sym
#                 hover --file <path>` mode listing every top-level symbol
#                 in the file). Pass through silently.
#   2. BLOCK   -- file is a known source extension, repo is indexed, the
#                 file has > THRESHOLD lines, and Read has no `limit` (or
#                 limit > THRESHOLD). Reason text directs the agent at sym
#                 + names the bypass mechanism.
#   3. BYPASS  -- LEGION_BYPASS_READ=1 env. Always allows; writes one
#                 telemetry row.
#
# Skip discipline:
# - LEGION_SKIP_PRE_READ_SYM=1: skip this hook specifically.
# - tool != Read: pass through.
# - file extension not in source set: pass through.
# - repo not legion-covered or not indexed: pass through.
# - limit set and <= 200: pass through (small targeted Reads encouraged).
# - file does not exist on disk: pass through (Read will surface its own error).
# - file is < THRESHOLD lines: pass through.
#
# Error handling: every legion invocation appends stderr to
# /tmp/legion-hook-errors.log; the hook always exits 0.

THRESHOLD=500
SMALL_READ_LIMIT=200

if [ "${LEGION_SKIP_PRE_READ_SYM:-}" = "1" ]; then
  echo "[legion-pre-read-sym] skipped (LEGION_SKIP_PRE_READ_SYM=1)" >&2
  exit 0
fi

# shellcheck source=lib/prelude.sh
source "${CLAUDE_PLUGIN_ROOT:-}/hooks/lib/prelude.sh" 2>/dev/null || exit 0

legion_hook_parse || exit 0

FILE_PATH=$(legion_hook_field '.tool_input.file_path')
LIMIT=$(legion_hook_field '.tool_input.limit')

if [ -z "$CWD" ] || [ "$TOOL" != "Read" ] || [ -z "$FILE_PATH" ]; then
  exit 0
fi

# Source extension check. List mirrors the SCIP indexer dispatch so an
# extension that isn't indexed never trips the block. Headers (.h, .hpp)
# included because scip-clang covers them.
EXT="${FILE_PATH##*.}"
case "$EXT" in
  rs|ts|tsx|js|jsx|py|go|java|rb|php|c|cc|cpp|cxx|h|hpp|cs) ;;
  *) exit 0 ;;
esac

if [ -z "$REPO" ]; then
  exit 0
fi

# Source the shared ladder library (indexed + telemetry + emit helpers).
# shellcheck source=_legion-prequery.sh
source "${CLAUDE_PLUGIN_ROOT}/hooks/_legion-prequery.sh"

# Universal gate.
legion_hook_covered || exit 0

# Block tier requires an index to redirect to.
if ! legion_indexed "$SESSION_ID" "$REPO"; then
  exit 0
fi

# Small-read carveout: any explicit limit <= 200 is always allowed. Agents
# doing targeted reads (specific line ranges, small chunks) are doing the
# right thing already; the hook is for unbounded full-file dumps.
if [ -n "$LIMIT" ] && [ "$LIMIT" != "null" ] && [ "$LIMIT" -le "$SMALL_READ_LIMIT" ] 2>/dev/null; then
  exit 0
fi

# Bypass tier: always wins. Telemetry row records the escape.
if [ "${LEGION_BYPASS_READ:-}" = "1" ]; then
  legion_prequery_record_bypass \
    "$REPO" "$SESSION_ID" "Read" "$FILE_PATH" "env:LEGION_BYPASS_READ=1" \
    "false" "false"
  exit 0
fi

# Need the file on disk to count lines. Read may resolve relative paths
# against the agent's CWD, so try both as-is and CWD-prefixed.
RESOLVED=""
if [ -f "$FILE_PATH" ]; then
  RESOLVED="$FILE_PATH"
elif [ -f "$CWD/$FILE_PATH" ]; then
  RESOLVED="$CWD/$FILE_PATH"
else
  exit 0
fi

# Line count. wc -l on a binary file is meaningless but the source-ext
# filter above already excluded those. Empty / unreadable returns 0.
LINES=$(wc -l < "$RESOLVED" 2>/dev/null | tr -d ' ')
if [ -z "$LINES" ] || [ "$LINES" -lt "$THRESHOLD" ] 2>/dev/null; then
  exit 0
fi

# If the agent set a limit > THRESHOLD that's still effectively unbounded
# for the cost-justification argument (we're trying to push toward sym,
# not to nag at a specific number).
if [ -n "$LIMIT" ] && [ "$LIMIT" != "null" ] && [ "$LIMIT" -le "$THRESHOLD" ] 2>/dev/null; then
  exit 0
fi

# State 2 BLOCK. Reason text intentionally inlines the path and line
# count so the agent has no ambiguity about which Read triggered it.
REASON="\`Read ${FILE_PATH}\` would bill ${LINES} lines x cache_read. This repo has a SCIP index; consider:

- \`legion sym hover <Symbol>\` -- one symbol's signature + docstring, in bytes.
- \`legion sym def <Symbol>\` -- jump straight to the definition site.
- \`Read ${FILE_PATH} limit=200\` -- targeted chunk if you really need source bytes.

If none of those answer your question (e.g. you need to see how multiple symbols interact, or the file is configuration not code), bypass with:

- \`LEGION_BYPASS_READ=1 Read ${FILE_PATH}\`

The bypass writes one row to bypass.jsonl so #440's summary will see it."

emit_deny "$REASON"
