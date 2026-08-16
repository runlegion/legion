#!/bin/bash
# Shared hook output emission (#614). The decision vocabulary is a closed
# set; before this file it was hand-built jq at 11 sites in two dialects.
#
# Event-shape contract (per Claude Code hook docs):
#
#   PreToolUse  -- hookSpecificOutput.permissionDecision allow|deny is the
#                  documented shape. The legacy top-level {decision:block}
#                  dialect for PreToolUse is migrated behind emit_deny.
#   Stop        -- top-level {decision:block, reason} IS the documented
#                  refusal shape for Stop (it is not legacy there); soft
#                  feedback goes through additionalContext (emit_context).
#   SessionStart / UserPromptSubmit / PostToolUse / SubagentStop /
#   Stop-nudge -- hookSpecificOutput.additionalContext (emit_context).
#                 PostToolUse support verified empirically on CC 2.1.233
#                 (#941: a mid-run post injected via a PostToolUse hook
#                 reached the model, stream-json captured).
#
# precompact.sh deliberately does NOT use this file: its PreCompact block
# reason is shown to the USER and must survive a missing jq, so it stays
# a static heredoc there.

# Double-source guard.
if [ -n "${LEGION_EMIT_SOURCED:-}" ]; then
  return 0
fi
LEGION_EMIT_SOURCED=1

# emit_allow CTX [REASON] -- PreToolUse: allow the call and inject CTX as
# additionalContext.
emit_allow() {
  local ctx="$1" reason="${2:-legion context injected}"
  jq -n --arg ctx "$ctx" --arg reason "$reason" '{
    "hookSpecificOutput": {
      "hookEventName": "PreToolUse",
      "permissionDecision": "allow",
      "permissionDecisionReason": $reason,
      "additionalContext": $ctx
    }
  }'
}

# emit_deny REASON [CTX] -- PreToolUse: refuse the tool call. REASON is
# the message the agent reads; CTX optionally injects additionalContext
# alongside the refusal.
emit_deny() {
  local reason="$1" ctx="${2:-}"
  if [ -n "$ctx" ]; then
    jq -n --arg reason "$reason" --arg ctx "$ctx" '{
      "hookSpecificOutput": {
        "hookEventName": "PreToolUse",
        "permissionDecision": "deny",
        "permissionDecisionReason": $reason,
        "additionalContext": $ctx
      }
    }'
  else
    jq -n --arg reason "$reason" '{
      "hookSpecificOutput": {
        "hookEventName": "PreToolUse",
        "permissionDecision": "deny",
        "permissionDecisionReason": $reason
      }
    }'
  fi
}

# emit_rewrite CMD CTX [REASON] -- PreToolUse: allow the call but REPLACE
# the tool's command with CMD via updatedInput (#827).
#
# The rewrite is only honest when the translation is LOSSLESS. A caller
# that would have to drop a flag the target command cannot express must
# emit_deny instead -- silently running something the agent did not ask
# for is worse than refusing, because the agent reads the result as the
# answer to its original question.
#
# CTX is mandatory rather than optional on purpose: a silent rewrite
# teaches nothing, so the agent re-derives the same wrong habit next
# session. Announcing the translation costs one line and the surface
# gets learned.
emit_rewrite() {
  local cmd="$1" ctx="$2"
  local reason="${3:-legion rewrote this to its audited equivalent}"

  # updatedInput is a WHOLE-OBJECT replacement for tool_input, not a
  # field-level merge. An earlier version of this helper emitted a bare
  # `{"command": $cmd}`, which silently dropped every other field the caller
  # sent -- for Bash that is `description`, `timeout` and
  # `run_in_background`, so a rewritten background command came back as a
  # FOREGROUND one and a long-running command lost its raised timeout. The
  # rewrite stayed correct and its context stayed wrong, which is the worst
  # shape for a translation the agent is told to trust.
  #
  # We patch the original tool_input instead of rebuilding it. That is also
  # correct if the platform ever merges rather than replaces -- resending a
  # field its own value is a no-op either way -- so this does not depend on
  # which semantics hold. INPUT is the raw payload captured by
  # legion_hook_parse; the fallback covers a caller that emits without
  # having parsed (tests, mainly), and keeps this helper total.
  local updated
  updated=$(printf '%s' "${INPUT:-}" \
    | jq -c --arg cmd "$cmd" '(.tool_input // {}) | .command = $cmd' 2>/dev/null)
  if [ -z "$updated" ]; then
    updated=$(jq -nc --arg cmd "$cmd" '{ "command": $cmd }')
  fi

  jq -n --arg ctx "$ctx" --arg reason "$reason" --argjson updated "$updated" '{
    "hookSpecificOutput": {
      "hookEventName": "PreToolUse",
      "permissionDecision": "allow",
      "permissionDecisionReason": $reason,
      "updatedInput": $updated,
      "additionalContext": $ctx
    }
  }'
}

# emit_block REASON -- top-level decision:block. The documented refusal
# shape for Stop events (hard gate with the harness 8-block cap as its
# backstop). Do NOT use for PreToolUse -- that dialect is deprecated
# there; use emit_deny.
emit_block() {
  jq -n --arg reason "$1" '{
    "decision": "block",
    "reason": $reason
  }'
}

# emit_context EVENT CTX -- additionalContext-only injection for EVENT
# (SessionStart, UserPromptSubmit, SubagentStop, Stop). Non-error feedback
# that continues the turn.
emit_context() {
  local event="$1" ctx="$2"
  jq -n --arg event "$event" --arg ctx "$ctx" '{
    "hookSpecificOutput": {
      "hookEventName": $event,
      "additionalContext": $ctx
    }
  }'
}
