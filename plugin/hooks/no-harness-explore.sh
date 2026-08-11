#!/bin/bash
# Legion PreToolUse hook: rewrite the harness built-in `Explore` subagent
# spawn to the `legion:legion-explore` plugin agent.
#
# The built-in Explore agent greps and reads raw files. On a legion-covered
# repo that is the wrong instrument: legion:legion-explore orients through SCIP
# sym queries (def/refs/impl/hover) and recall/consult, returning conclusions
# with file:line evidence instead of file dumps. Swapping subagent_type is a
# single argument substitution on the SAME Task/Agent call -- exactly what
# `updatedInput` exists for (the pattern `no-git-push.sh` established for
# #827) -- so this rewrites the spawn instead of refusing it.
#
# Mechanism note: PreToolUse is the only event that can alter a spawn before
# it happens (permissionDecision allow + updatedInput). SubagentStart matches
# on agent type but is context-only -- it cannot rewrite or deny -- so the
# redirect must run here, before the spawn, on the Agent/Task tool call that
# carries `subagent_type`.
#
# `updatedInput` REPLACES the whole tool_input object, not just the changed
# key, so the rewrite is built from the ORIGINAL tool_input with only
# `.subagent_type` overwritten -- `prompt`, `description`, and any other
# field (isolation, model, name) carry through unmodified. A rewrite that
# dropped `prompt` would spawn legion-explore with no instructions at all,
# which is worse than the refusal it replaces -- that is the "lossless"
# requirement `emit_rewrite`'s doc comment names, and it is why this hook
# builds updatedInput by hand instead of reusing `emit_rewrite`: that helper
# hardcodes the Bash `command` key and has no shape for a Task-tool field
# substitution.
#
# legion-explore's contract differs from Explore's: it returns conclusions
# with file:line citations, never raw file dumps, and orients through sym/
# recall rather than grep/find. The prompt itself is left untouched -- an
# automatic rewrite risks losing what the caller actually asked for -- but
# the substitution is announced via additionalContext so the calling agent
# reads the result under the right conventions instead of expecting an
# Explore-shaped answer.
#
# What this rewrites:
#   - An Agent/Task tool call whose tool_input.subagent_type is "Explore"
#     (case-insensitive exact match), in a legion-covered repo.
#
# What this does NOT touch:
#   - legion:legion-explore / legion-explore (the redirect target).
#   - Any other subagent type (Plan, general-purpose, custom agents).
#   - Repos legion does not cover (the universal coverage gate).
#
# No bypass env: an agent that genuinely needs raw exploration can name a
# different agent (general-purpose) explicitly rather than asking for
# Explore by name.

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

TARGET="legion:legion-explore"

# The full original tool_input with only subagent_type overwritten -- see
# the header note on why updatedInput must carry the WHOLE object.
UPDATED_INPUT=$(echo "$INPUT" | jq -c --arg target "$TARGET" '.tool_input | .subagent_type = $target')

jq -n \
  --argjson input "$UPDATED_INPUT" \
  --arg reason "legion rewrote this spawn to ${TARGET}" \
  --arg ctx "Rewrote subagent_type from \"${SUBAGENT}\" to \"${TARGET}\" -- same prompt, same description, only the agent changed.

The built-in Explore subagent greps and reads raw files. legion:legion-explore orients through SCIP sym queries (def/refs/impl/hover) and recall/consult instead, and returns conclusions with file:line evidence rather than file dumps. Read its result as a synthesized answer with citations, not a listing -- if your prompt asked for something Explore-shaped (e.g. a raw file dump), the returned shape will differ." \
  '{
    "hookSpecificOutput": {
      "hookEventName": "PreToolUse",
      "permissionDecision": "allow",
      "permissionDecisionReason": $reason,
      "updatedInput": $input,
      "additionalContext": $ctx
    }
  }'
