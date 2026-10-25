#!/bin/bash
# Legion SubagentStop hook (#570, #752, #1441).
#
# Injects a nudge via hookSpecificOutput.additionalContext -- "non-error
# feedback that continues the turn" (CC 2.1.168). The turn it continues is the
# SUBAGENT's own: the subagent was about to stop with its real deliverable as
# its last message, this nudge reopens that turn for one more message, and
# THAT message is what the Task tool actually returns to the parent as the
# final result. So the nudge orders the subagent to RESTATE its complete
# deliverable as the body of that final message, never a bare acknowledgment
# that points back at a message the parent cannot see (#752).
#
# It stores nothing in legion memory. It once saved the last 1500 chars of
# every subagent transcript as a checkpoint reflection; those were mostly
# fragments ("Now write tests.") that buried real reflections and displaced
# the session's own checkpoint at boot (#1440, #1441). The parent already
# receives the deliverable through the restatement.
#
# Never blocks the parent: every failure path exits 0.

# shellcheck source=lib/prelude.sh
source "${CLAUDE_PLUGIN_ROOT:-}/hooks/lib/prelude.sh" 2>/dev/null || exit 0
# shellcheck source=lib/emit.sh
source "${CLAUDE_PLUGIN_ROOT:-}/hooks/lib/emit.sh" 2>/dev/null || exit 0

legion_hook_parse || exit 0
if [ -z "$CWD" ]; then
  exit 0
fi

AGENT_TYPE=$(echo "$INPUT" | jq -r '.agent_type // "subagent"' 2>/dev/null)
TRANSCRIPT=$(legion_hook_field '.agent_transcript_path')

# Loop guard + idempotency (#584). The additionalContext emitted below
# continues the subagent's own turn; without a guard a re-delivered
# SubagentStop event re-injects context, looping and burning tokens
# (observed 3x, ~337k tokens on one spurious fire). Two guards,
# mirroring stop.sh:
#   1. stop_hook_active -- skip a continuation we ourselves caused.
#   2. A per-event marker keyed on the subagent identity (its transcript path,
#      else session_id + agent_type) so each subagent stop is processed once.
STOP_ACTIVE=$(echo "$INPUT" | jq -r '.stop_hook_active // false' 2>/dev/null)
if [ "$STOP_ACTIVE" = "true" ]; then
  exit 0
fi

DEDUP_SRC="${TRANSCRIPT:-${SESSION_ID}:${AGENT_TYPE}}"
DEDUP_KEY=$(legion_hash_str "$DEDUP_SRC")
MARKER="${TMPDIR:-/tmp}/legion-subagent-stop-${DEDUP_KEY}"
if [ -f "$MARKER" ]; then
  # Already processed this subagent stop. Re-injecting
  # additionalContext is exactly what loops, so pass through silently.
  exit 0
fi
# Claim the event before doing work so a fast re-delivery cannot double-fire.
: > "$MARKER" 2>/dev/null || true

# Reopen the subagent's own turn for one more message (#752). This message,
# not the one before it, is what the Task tool hands back to the parent -- so
# it must never be a bare acknowledgment, and never point back at a prior
# message, because the parent cannot see any message but this one.
CTX="This is a final-message nudge, not a request for new work. Your NEXT message is the ONLY message the orchestrator that spawned you will ever receive -- anything not repeated in it is permanently lost, including whatever you already reported in an earlier message. So: RESTATE your complete deliverable in full as the body of your next message, exactly as if reporting it for the first time. Never write \"see above\", \"already delivered\", or any other reference to a prior message."

emit_context "SubagentStop" "$CTX" 2>/dev/null || true

exit 0
