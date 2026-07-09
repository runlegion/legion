#!/bin/bash
# Legion SubagentStop hook (#570).
#
# When a spawned subagent ends, its work otherwise lives only in the parent's
# context -- nothing durable reaches legion memory, and a long delegated task
# vanishes the moment the parent's session compacts or ends. This hook:
#
#   1. Persists the subagent's final output as a domain=checkpoint reflection
#      (the unified resume-anchor, #568), tagged subagent,auto so it is
#      distinguishable from a deliberate /checkpoint and ages out via decay.
#   2. Injects a nudge via hookSpecificOutput.additionalContext -- "non-error
#      feedback that continues the turn" (CC 2.1.168). The turn it continues
#      is the SUBAGENT's own: the subagent was about to stop with its real
#      deliverable as its last message, this nudge reopens that turn for one
#      more message, and THAT message is what the Task tool actually returns
#      to the parent as the final result. A prior wording here ("Its work is
#      checkpointed... recall with...") caused the subagent to answer the
#      nudge instead of restating its findings, so the parent received a bare
#      acknowledgment ("already delivered above") while the real deliverable
#      was stranded in a turn the parent can never see (#752). The prompt
#      below fixes that: it orders the subagent to RESTATE its complete
#      deliverable as the body of this final message FIRST, and append the
#      checkpoint note only after, so the one message the parent actually
#      receives is never just an ack.
#
# Never blocks the parent: every failure path exits 0. Cheap by design
# (bounded transcript tail) to stay well under the hook timeout.

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

if [ ! -x "$LEGION" ]; then
  exit 0
fi

# Loop guard + idempotency (#584). The additionalContext emitted below
# continues the subagent's own turn; without a guard a re-delivered
# SubagentStop event re-reflects and re-injects context, looping and burning
# tokens (observed 3x, ~337k tokens on one spurious fire). Two guards,
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
  # Already processed this subagent stop. Re-reflecting and re-injecting
  # additionalContext is exactly what loops, so pass through silently.
  exit 0
fi
# Claim the event before doing work so a fast re-delivery cannot double-fire.
: > "$MARKER" 2>/dev/null || true

# Scrape the subagent's final output from its transcript tail. Same extraction
# shape as precompact.sh, with the alternate-format fallback. Bounded to the
# last ~1500 chars to keep the reflection (and this hook) small.
SUMMARY=""
if [ -n "$TRANSCRIPT" ] && [ -f "$TRANSCRIPT" ]; then
  SUMMARY=$(tail -200 "$TRANSCRIPT" 2>/dev/null | \
    jq -r 'select(.type == "assistant") | .message.content[]? | select(.type == "text") | .text // empty' 2>/dev/null | \
    tail -c 1500)
  if [ -z "$SUMMARY" ]; then
    SUMMARY=$(tail -200 "$TRANSCRIPT" 2>/dev/null | \
      jq -r 'select(.role == "assistant") | .content // empty' 2>/dev/null | \
      tail -c 1500)
  fi
fi

# Persist the subagent checkpoint. Fail-open: a reflect error must never block
# the parent, so swallow it (|| true). No transcript text -> skip the reflect
# entirely rather than store an empty marker; the nudge below still fires so
# the subagent's final message (and thus the parent) is never silently empty.
if [ -n "$SUMMARY" ]; then
  "$LEGION" reflect --repo "$REPO" --domain checkpoint --tags subagent,auto \
    --text "[SUBAGENT CHECKPOINT] ${AGENT_TYPE}: ${SUMMARY}" >/dev/null 2>&1 || true
fi

# Reopen the subagent's own turn for one more message (#752). This message,
# not the one before it, is what the Task tool hands back to the parent -- so
# it must never be a bare acknowledgment. Order matters: restate the full
# deliverable FIRST, checkpoint note SECOND, and never point back at a prior
# message, because the parent cannot see any message but this one.
CTX="This is a checkpoint nudge, not a request for new work. Your NEXT message is the ONLY message the orchestrator that spawned you will ever receive -- anything not repeated in it is permanently lost, including whatever you already reported in an earlier message. So: RESTATE your complete deliverable in full as the body of your next message, exactly as if reporting it for the first time. Never write \"see above\", \"already delivered\", or any other reference to a prior message. Only after the full restatement, append one line noting that this ${AGENT_TYPE} run is checkpointed in legion (domain=checkpoint); recall with: legion recall --repo ${REPO} --domain checkpoint --limit 1."

emit_context "SubagentStop" "$CTX" 2>/dev/null || true

exit 0
