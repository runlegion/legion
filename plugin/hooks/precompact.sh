#!/bin/bash
# Legion PreCompact hook: block auto-compaction and prompt the model to run
# /snooze then /clear. Auto-compaction is mechanical -- /snooze consolidates
# the session into legion memory (boost, summary, bullpen), then /clear starts
# fresh. The checkpoint reflection below is a safety net in case the block
# is overridden.

# Hook subshells do not inherit the plugin bin dir on PATH -- only the Bash
# tool does. Invoke via full CLAUDE_PLUGIN_ROOT path (fixes #204).
LEGION="${CLAUDE_PLUGIN_ROOT}/bin/legion"
LOG=/tmp/legion-hook-errors.log

INPUT=$(cat)
CWD=$(echo "$INPUT" | jq -r '.cwd // empty')
TRANSCRIPT=$(echo "$INPUT" | jq -r '.transcript_path // empty')

if [ -z "$CWD" ]; then
  exit 0
fi

REPO=$(basename "$CWD")
CWD_HASH=$(echo "$CWD" | md5 -q 2>/dev/null || echo "$CWD" | md5sum 2>/dev/null | cut -d' ' -f1)
CHECKPOINT_MARKER="/tmp/legion-checkpoint-failed-${CWD_HASH}"

# Extract recent assistant text from transcript JSONL as a safety-net checkpoint
CONTEXT=""
if [ -n "$TRANSCRIPT" ] && [ -f "$TRANSCRIPT" ]; then
  CONTEXT=$(tail -200 "$TRANSCRIPT" 2>/dev/null | \
    jq -r 'select(.type == "assistant") | .message.content[]? | select(.type == "text") | .text // empty' 2>/dev/null | \
    tail -c 2000)
fi

if [ -z "$CONTEXT" ]; then
  CONTEXT=$(tail -200 "$TRANSCRIPT" 2>/dev/null | \
    jq -r 'select(.role == "assistant") | .content // empty' 2>/dev/null | \
    tail -c 2000)
fi

if [ -z "$CONTEXT" ]; then
  CONTEXT="(no transcript context extracted)"
fi

# Safety-net checkpoint reflection in case the model bypasses the block.
# See #209 -- losing a checkpoint silently is what caused the original incident.
if ! "$LEGION" reflect --repo "$REPO" --text "[COMPACT CHECKPOINT] Work in progress before compaction: ${CONTEXT}" --domain "checkpoint" --tags "auto,precompact" 2>>"$LOG"; then
  touch "$CHECKPOINT_MARKER" 2>/dev/null
else
  rm -f "$CHECKPOINT_MARKER" 2>/dev/null
fi

# Block auto-compaction and direct the model to consolidate properly.
jq -n '{
  "decision": "block",
  "reason": "Context is about to auto-compact, which loses everything not in the transcript tail. Do not let it. Run /snooze to consolidate this session into legion memory (review decisions, boost reflections that helped, write a domain=snooze summary the next session will recall, cross-pollinate to the bullpen), then run /clear to start fresh. A checkpoint reflection has been written as a fallback, but it is mechanical -- /snooze captures the why."
}'
