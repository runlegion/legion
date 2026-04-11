#!/bin/bash
# Legion PreCompact hook: auto-reflect a checkpoint before context compaction
# Extracts recent assistant output from the transcript and saves it as a
# reflection so that the post-compact SessionStart hook can recall it.

# Hook subshells do not inherit the plugin bin dir on PATH -- only the Bash
# tool does. Invoke via full CLAUDE_PLUGIN_ROOT path (fixes #204).
LEGION="${CLAUDE_PLUGIN_ROOT}/bin/legion"

INPUT=$(cat)
CWD=$(echo "$INPUT" | jq -r '.cwd // empty')
TRANSCRIPT=$(echo "$INPUT" | jq -r '.transcript_path // empty')

if [ -z "$CWD" ]; then
  exit 0
fi

REPO=$(basename "$CWD")

# Extract recent assistant text from transcript JSONL
CONTEXT=""
if [ -n "$TRANSCRIPT" ] && [ -f "$TRANSCRIPT" ]; then
  # Grab last 200 lines, pull assistant text content, keep last ~2000 chars
  CONTEXT=$(tail -200 "$TRANSCRIPT" 2>/dev/null | \
    jq -r 'select(.type == "assistant") | .message.content[]? | select(.type == "text") | .text // empty' 2>/dev/null | \
    tail -c 2000)
fi

# Fallback: try alternate JSONL format
if [ -z "$CONTEXT" ]; then
  CONTEXT=$(tail -200 "$TRANSCRIPT" 2>/dev/null | \
    jq -r 'select(.role == "assistant") | .content // empty' 2>/dev/null | \
    tail -c 2000)
fi

if [ -z "$CONTEXT" ]; then
  CONTEXT="(no transcript context extracted)"
fi

# Save checkpoint reflection
"$LEGION" reflect --repo "$REPO" --text "[COMPACT CHECKPOINT] Work in progress before compaction: ${CONTEXT}" --domain "checkpoint" --tags "auto,precompact" 2>/dev/null

exit 0
