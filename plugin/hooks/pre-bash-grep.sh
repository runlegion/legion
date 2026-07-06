#!/bin/bash
# Legion PreToolUse hook (#438): when a Bash command starts with a search
# binary (grep|rg|ag|ack|find|fd), apply the three-state ladder:
#
#   1. INJECT  -- repo not indexed or no high-confidence sym hit.
#                 Emit additionalContext with whatever sym found.
#   2. BLOCK   -- repo indexed AND `legion sym def` returned >=1 result.
#                 Block the Bash call; the agent should call sym instead.
#   3. SOFT BYPASS -- LEGION_BYPASS_GREP=1 env or `# legion-bypass:`
#                 sentinel, for free-text searches. REFUSED for
#                 symbol-shaped patterns with a local hit. There is NO
#                 hard escape (#560): mandatory shell-grep blocking is the
#                 operator's permissions.deny, not this hook.
#
# Sibling of pre-grep.sh which covers the Grep and Glob tools. This hook is now SOFT FALLBACK GUIDANCE: the
# mandatory shell-grep block is the operator's settings.json
# permissions.deny (Bash(grep:*)/Bash(rg:*)/...), which is evaluated
# before this hook runs and inherits to subagents. See
# docs/decisions/2026-06-02-grep-blocking-is-operator-permissions.md.
# This hook still injects sym context and refuses the soft bypass on a
# local symbol hit, for repos/operators that have not set the deny rule.
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

# shellcheck source=lib/prelude.sh
source "${CLAUDE_PLUGIN_ROOT:-}/hooks/lib/prelude.sh" 2>/dev/null || exit 0

legion_hook_parse || exit 0

COMMAND=$(legion_hook_field '.tool_input.command')

if [ -z "$CWD" ] || [ "$TOOL" != "Bash" ] || [ -z "$COMMAND" ]; then
  exit 0
fi

if [ -z "$REPO" ]; then
  exit 0
fi

# Source the shared ladder library. It sources _legion-indexed.sh and
# lib/emit.sh transitively.
# shellcheck source=_legion-prequery.sh
source "${CLAUDE_PLUGIN_ROOT}/hooks/_legion-prequery.sh"

# Universal gate: skip uncovered repos.
legion_hook_covered || exit 0

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
# refusal points the agent at sym. There is NO env-var hard escape: a
# frictionless LEGION_BYPASS_GREP_HARD only made enforcement optional
# (#560). Mandatory shell-grep blocking is the operator's permissions.deny
# (docs/decisions/2026-06-02-grep-blocking-is-operator-permissions.md); this
# hook is soft fallback guidance for operators who have not set the deny rule.
BYPASS_REASON=$(legion_prequery_bypass_reason "$COMMAND")

if [ -n "$BYPASS_REASON" ]; then
  # Refuse the soft bypass if the pattern matches a real local symbol.
  if [ -n "$LOCAL_HITS" ] && [ "$LOCAL_HITS" != "[]" ]; then
    REASON="Soft bypass refused: \`${PATTERN}\` resolves to a symbol in this repo's SCIP index. Use \`legion sym def ${PATTERN} --repo ${REPO}\` (or \`sym refs\` / \`sym hover\`) instead. The \`# legion-bypass:\` sentinel exists for free-text searches; it cannot route around sym for symbol queries.

\`legion sym def ${PATTERN} --repo ${REPO}\` returned:

\`\`\`json
${LOCAL_HITS}
\`\`\`

For symbols, \`legion sym def ${PATTERN}\` / \`sym refs\` / \`sym list\` answer in bytes -- sym covers every indexed language, not just Rust. For non-symbol shapes: \`legion sym etc find-content '${PATTERN}' --repo ${REPO}\` (exact/regex content, the sanctioned grep), \`legion sym tree --repo ${REPO}\` (file/dir structure), \`legion sym etc extract <path> --field <field>\` (one config/frontmatter value without a full read), \`legion sym etc find-file '${PATTERN}' --repo ${REPO}\` (locate a file by name/role). Reach for the Grep tool only if none of those answer it. Your operator may block shell ${BINARY} outright via permissions.deny; that is the intended mandatory gate, and there is no env-var escape from it."
    emit_deny "$REASON"
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

The soft bypass (\`# legion-bypass: <reason>\` or LEGION_BYPASS_GREP=1) is REFUSED for symbol-shaped patterns that resolve in this repo's SCIP index -- the sentinel exists for free-text searches, not for symbol queries dressed up as text. For symbols use \`legion sym def ${PATTERN}\` / \`sym refs\` / \`sym list\` -- sym covers every indexed language, not just Rust. For non-symbol shapes: \`legion sym etc find-content '${PATTERN}' --repo ${REPO}\` (content), \`legion sym tree --repo ${REPO}\` (structure), \`legion sym etc extract <path> --field <field>\` (a config/frontmatter value), \`legion sym etc find-file '${PATTERN}' --repo ${REPO}\` (locate by name/role). Reach for the Grep tool only if none of those answer it. There is no env-var hard escape -- the mandatory shell-grep block is the operator's permissions.deny."
    emit_deny "$REASON"
    exit 0
  fi
fi

# State 1 INJECT: not indexed but we did find something via the cluster
# (sym pulls from every stored index, not just the current repo). Emit
# the hits as additionalContext so the agent can decide.
CTX="## Legion sym for \`${PATTERN}\` (Bash ${BINARY})

\`legion sym def ${PATTERN}\` returned (from other repos' indexes -- ${REPO} has no local SCIP index yet, so the block tier is disabled here):

\`\`\`json
${HITS}
\`\`\`

For ${REPO} itself, non-symbol shapes still have a sanctioned answer that needs no SCIP index: \`legion sym etc find-content '${PATTERN}' --repo ${REPO}\` (content), \`legion sym tree --repo ${REPO}\` (structure), \`legion sym etc find-file '${PATTERN}' --repo ${REPO}\` (locate by name). Run \`legion index ${REPO}\` to get a real local \`sym def\` answer too."
emit_allow "$CTX" "legion sym/recall results injected"
