#!/bin/bash
# Legion PreToolUse hook (#438): when a Bash command starts with a search
# binary (grep|rg|ag|ack|find|fd), apply the three-state ladder:
#
#   1. INJECT  -- repo not indexed or no high-confidence sym hit.
#                 Emit additionalContext with whatever sym found.
#   2. BLOCK   -- repo indexed AND `legion sym def` returned >=1 result.
#                 Block the Bash call; the agent should call sym instead.
#   3. BYPASS  -- LEGION_BYPASS_GREP=1 env or `# legion-bypass:` sentinel
#                 substring in the command. Always allows; logs telemetry.
#
# Sibling of pre-grep-scip.sh and pre-grep-recall.sh which cover the
# Grep and Glob tools. This hook closes the Bash escape gap demonstrated
# in #438: agents typing `grep -r ...` via Bash slipped past the existing
# tool-matched hooks entirely.
#
# Skip discipline:
# - LEGION_SKIP_PRE_BASH_GREP=1: skip this hook specifically.
# - command does not start with a covered search binary: pass through.
# - repo not legion-covered: pass through (universal hook gate).
# - extracted pattern not symbol-shaped: pass through (regex/free text
#   queries aren't sym lookups).
# - missing legion binary: pass through.
#
# Error handling: every legion invocation appends stderr to
# /tmp/legion-hook-errors.log; the hook always exits 0.

if [ "${LEGION_SKIP_PRE_BASH_GREP:-}" = "1" ]; then
  echo "[legion-pre-bash-grep] skipped (LEGION_SKIP_PRE_BASH_GREP=1)" >&2
  exit 0
fi

INPUT=$(cat)
if [ -z "$INPUT" ]; then
  exit 0
fi

CWD=$(echo "$INPUT" | jq -r '.cwd // empty' 2>/dev/null)
TOOL=$(echo "$INPUT" | jq -r '.tool_name // empty' 2>/dev/null)
COMMAND=$(echo "$INPUT" | jq -r '.tool_input.command // empty' 2>/dev/null)
SESSION_ID=$(echo "$INPUT" | jq -r '.session_id // empty' 2>/dev/null)

if [ -z "$CWD" ] || [ "$TOOL" != "Bash" ] || [ -z "$COMMAND" ]; then
  exit 0
fi

REPO="${LEGION_REPO:-$(basename "$CWD")}"
if [ -z "$REPO" ]; then
  exit 0
fi

# Source the shared library. It sources _legion-covered.sh and
# _legion-indexed.sh transitively.
# shellcheck source=_legion-prequery.sh
source "${CLAUDE_PLUGIN_ROOT}/hooks/_legion-prequery.sh"

# Universal gate: skip uncovered repos.
if ! legion_covered "$SESSION_ID" "$REPO"; then
  exit 0
fi

# Detect leading search binary; pass through if none.
BINARY=$(legion_prequery_bash_binary "$COMMAND")
if [ -z "$BINARY" ]; then
  exit 0
fi

# Extract the pattern. Empty extraction means we couldn't isolate one;
# pass through rather than guessing.
PATTERN=$(legion_prequery_extract_pattern "$COMMAND" "$BINARY")
if [ -z "$PATTERN" ]; then
  exit 0
fi

# Probe sym before any decision so the bypass row carries the
# `had_sym_hits` signal that #440's summary will need.
HITS=$(legion_prequery_sym_def "$PATTERN")
HAD_SYM="false"
if [ -n "$HITS" ]; then
  HAD_SYM="true"
fi

# Pre-compute LOCAL_HITS so both the bypass refusal path and the
# State 2 BLOCK path can use them.
LOCAL_HITS=""
if [ "$HAD_SYM" = "true" ] && legion_indexed "$SESSION_ID" "$REPO"; then
  LOCAL_HITS=$(legion_prequery_filter_hits_local "$HITS" "$REPO")
fi

# State 3: bypass. Two tiers.
#
# Soft bypass (`# legion-bypass: <reason>` or LEGION_BYPASS_GREP=1) is
# REFUSED when the pattern resolves to a real symbol in THIS repo's
# SCIP index. The bypass sentinel exists for free-text searches; it
# cannot route around sym for symbol queries dressed up as text. The
# refusal points the agent at sym + names the LEGION_BYPASS_GREP_HARD
# hard escape for the rare case where the agent really does need a
# grep-style file scan over symbol-shaped text (e.g. counting call
# sites of a deprecated API across files outside the SCIP index).
#
# Hard bypass (LEGION_BYPASS_GREP_HARD=1) always allows but writes a
# row with bypass_class=hard so /telemetry summary can show how often
# the hard escape fires (loud signal that sym is under-serving).
BYPASS_REASON=$(legion_prequery_bypass_reason "$COMMAND")
HARD_BYPASS="${LEGION_BYPASS_GREP_HARD:-}"

if [ "$HARD_BYPASS" = "1" ]; then
  legion_prequery_record_bypass \
    "$REPO" "$SESSION_ID" "Bash" "$PATTERN" "hard:${BYPASS_REASON:-no-reason}" \
    "$HAD_SYM" "false"
  exit 0
fi

if [ -n "$BYPASS_REASON" ]; then
  # Refuse the soft bypass if the pattern matches a real local symbol.
  if [ -n "$LOCAL_HITS" ] && [ "$LOCAL_HITS" != "[]" ]; then
    REASON="Soft bypass refused: \`${PATTERN}\` resolves to a symbol in this repo's SCIP index. Use \`legion sym def ${PATTERN} --repo ${REPO}\` (or \`sym refs\` / \`sym hover\`) instead. The \`# legion-bypass:\` sentinel exists for free-text searches; it cannot route around sym for symbol queries.

\`legion sym def ${PATTERN} --repo ${REPO}\` returned:

\`\`\`json
${LOCAL_HITS}
\`\`\`

If you genuinely need ${BINARY} against a symbol-shaped pattern (counting call sites in files outside the SCIP index, comparing line-by-line content for a refactor diff), use the hard escape: \`LEGION_BYPASS_GREP_HARD=1 ${COMMAND}\`. The hard bypass writes a row to bypass.jsonl with \`bypass_class=hard\` so /telemetry summary can see how often it fires."
    legion_prequery_emit_block "$REASON"
    exit 0
  fi
  # Soft bypass allowed: pattern is free-text or has no local symbol hits.
  legion_prequery_record_bypass \
    "$REPO" "$SESSION_ID" "Bash" "$PATTERN" "$BYPASS_REASON" \
    "$HAD_SYM" "false"
  exit 0
fi

# No hits: state 1 INJECT path is empty -- nothing to inject. Pass through
# silently rather than emit a content-free additionalContext block (which
# would just bill cache_read for "no hits").
if [ "$HAD_SYM" != "true" ]; then
  exit 0
fi

# State 2 BLOCK: only when the repo actually has an index to redirect to.
# In an unindexed repo, the agent has no sym alternative, so we keep the
# softer State 1 (inject + nudge) shape.
if legion_indexed "$SESSION_ID" "$REPO"; then
  # LOCAL_HITS was pre-computed above for the bypass refusal path; reuse
  # the value here rather than calling the relevance gate twice.
  # Relevance gate (#458): cluster-wide sym hits in unrelated repos are
  # not a useful redirect for a grep targeting THIS repo. Common
  # dictionary words like `name`, `data`, `value`, `type`, `id` are
  # symbol-shaped and exist as identifiers in every codebase, but a
  # grep on the operator's TOML config in legion's repo should not be
  # blocked because huttspawn has a variable named `name`. The
  # pre-computed LOCAL_HITS reflects that filter.
  if [ -z "$LOCAL_HITS" ] || [ "$LOCAL_HITS" = "[]" ]; then
    # No relevant hits in this repo's index. Fall through to inject
    # below -- the cluster-wide hits may still be useful context but
    # don't justify blocking.
    :
  else
    REASON="Use \`legion sym def ${PATTERN} --repo ${REPO}\` -- it answered this in bytes from the SCIP index. Bash ${BINARY} on \`${PATTERN}\` would scan files and bill cache_read.

\`legion sym def ${PATTERN} --repo ${REPO}\` returned:

\`\`\`json
${LOCAL_HITS}
\`\`\`

The soft bypass (\`# legion-bypass: <reason>\` or LEGION_BYPASS_GREP=1) is REFUSED for symbol-shaped patterns that resolve in this repo's SCIP index -- the sentinel exists for free-text searches, not for symbol queries dressed up as text. If you genuinely need ${BINARY} against this symbol-shaped pattern (counting call sites in files outside the SCIP index, comparing line-by-line content for a refactor diff), use the hard escape:

\`LEGION_BYPASS_GREP_HARD=1 ${COMMAND}\`

The hard bypass writes a row to bypass.jsonl with \`bypass_class=hard\` so /telemetry summary can see how often sym is under-serving."
    legion_prequery_emit_block "$REASON"
    exit 0
  fi
fi

# State 1 INJECT: not indexed but we did find something via the cluster
# (sym pulls from every stored index, not just the current repo). Emit
# the hits as additionalContext so the agent can decide.
CTX="## Legion sym for \`${PATTERN}\` (Bash ${BINARY})

\`legion sym def ${PATTERN}\` returned:

\`\`\`json
${HITS}
\`\`\`

This repo has no SCIP index, so the block tier is disabled. Consider \`legion sym def ${PATTERN}\` instead of ${BINARY} on this pattern."
legion_prequery_emit_allow "$CTX"
