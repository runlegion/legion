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
#   SessionStart / UserPromptSubmit / SubagentStop / Stop-nudge --
#                  hookSpecificOutput.additionalContext (emit_context).
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
