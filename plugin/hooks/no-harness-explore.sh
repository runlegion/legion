#!/bin/bash
# Legion PreToolUse hook: block the harness built-in `Explore` subagent and
# redirect to the `legion:legion-explore` plugin agent.
#
# The built-in Explore agent greps and reads raw files. On a legion-covered
# repo that is the wrong instrument: legion:legion-explore orients through SCIP
# sym queries (def/refs/impl/hover) and recall/consult, returning conclusions
# with file:line evidence instead of file dumps. This hook denies an Explore
# spawn and names the replacement.
#
# Mechanism note: PreToolUse is the only event that can BLOCK a spawn
# (permissionDecision deny). SubagentStart matches on agent type but is
# context-only -- it cannot deny -- so the redirect must run here, before the
# spawn, on the Agent/Task tool call that carries `subagent_type`.
#
# What this blocks:
#   - An Agent/Task tool call whose tool_input.subagent_type is "Explore"
#     (case-insensitive exact match), in a legion-covered repo.
#
# What this does NOT block:
#   - legion:legion-explore / legion-explore (the redirect target).
#   - Any other subagent type (Plan, general-purpose, custom agents).
#   - Repos legion does not cover (the universal coverage gate).
#
# No bypass env: an agent that genuinely needs raw exploration can name a
# different agent (general-purpose) or use legion:legion-explore, which itself
# falls back to bounded Reads. The point is to retire the grep-first default,
# not to forbid all exploration.

# shellcheck source=lib/prelude.sh
source "${CLAUDE_PLUGIN_ROOT:-}/hooks/lib/prelude.sh" 2>/dev/null || exit 0
# shellcheck source=lib/emit.sh
source "${CLAUDE_PLUGIN_ROOT:-}/hooks/lib/emit.sh" 2>/dev/null || exit 0

legion_hook_parse || exit 0

SUBAGENT=$(legion_hook_field '.tool_input.subagent_type')
if [ -z "$SUBAGENT" ] || [ "$SUBAGENT" = "null" ]; then
  exit 0
fi

# Case-insensitive EXACT match on "explore". An exact match must not catch the
# redirect target (legion-explore / legion:legion-explore), so compare the whole
# lowercased value, not a substring.
SUBAGENT_LC=$(printf '%s' "$SUBAGENT" | tr '[:upper:]' '[:lower:]')
if [ "$SUBAGENT_LC" != "explore" ]; then
  exit 0
fi

# Universal gate: only enforce where legion is the intended tool (#353).
legion_hook_covered || exit 0

emit_deny "The built-in \`Explore\` subagent is disabled on legion-covered repos. Use \`legion:legion-explore\` instead -- it orients through SCIP sym queries (def/refs/impl/hover) and recall/consult rather than grep/find, and returns conclusions with file:line evidence instead of file dumps.

Re-issue the call with subagent_type \`legion:legion-explore\` (same prompt)."
