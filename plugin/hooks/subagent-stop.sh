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
#   2. Injects a one-line pointer to the PARENT via
#      hookSpecificOutput.additionalContext. Verified against CC 2.1.168:
#      SubagentStop additionalContext is delivered to the parent (the spawning
#      agent), and is "non-error feedback that continues the conversation" --
#      the parent was resuming after the subagent anyway, so this just tells it
#      the work is checkpointed and recallable.
#
# Never blocks the parent: every failure path exits 0. Cheap by design
# (bounded transcript tail) to stay well under the hook timeout.

INPUT=$(cat)
CWD=$(echo "$INPUT" | jq -r '.cwd // empty' 2>/dev/null)
if [ -z "$CWD" ]; then
  exit 0
fi

REPO="${LEGION_REPO:-$(basename "$CWD")}"
AGENT_TYPE=$(echo "$INPUT" | jq -r '.agent_type // "subagent"' 2>/dev/null)
TRANSCRIPT=$(echo "$INPUT" | jq -r '.agent_transcript_path // empty' 2>/dev/null)

LEGION_BIN="${CLAUDE_PLUGIN_ROOT}/bin/legion"
if [ ! -x "$LEGION_BIN" ]; then
  exit 0
fi

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
# entirely rather than store an empty marker; the parent pointer below still
# fires so the parent knows the subagent ended.
if [ -n "$SUMMARY" ]; then
  "$LEGION_BIN" reflect --repo "$REPO" --domain checkpoint --tags subagent,auto \
    --text "[SUBAGENT CHECKPOINT] ${AGENT_TYPE}: ${SUMMARY}" >/dev/null 2>&1 || true
fi

# Inform the parent. additionalContext continues the parent turn (it was
# resuming regardless); point it at the persisted artifact rather than
# restating the subagent's output, which the parent already received.
CTX="Subagent ${AGENT_TYPE} finished. Its work is checkpointed in legion (domain=checkpoint); recall with: legion recall --repo ${REPO} --domain checkpoint --limit 1."

jq -n --arg ctx "$CTX" '{
  "hookSpecificOutput": {
    "hookEventName": "SubagentStop",
    "additionalContext": $ctx
  }
}' 2>/dev/null || true

exit 0
