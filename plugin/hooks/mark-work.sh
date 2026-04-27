#!/bin/bash
# Legion PostToolUse hook: mark this session as having done real work so the
# Stop hook fires its reflect prompt only when there is something to reflect
# on. Prose-only Q&A sessions (no tool calls) skip the prompt.
#
# Touches /tmp/legion-work-<md5(cwd)>. The Stop hook reads this marker.

INPUT=$(cat)
CWD=$(echo "$INPUT" | jq -r '.cwd // empty')

if [ -z "$CWD" ]; then
  exit 0
fi

CWD_HASH=$(echo "$CWD" | md5 -q 2>/dev/null || echo "$CWD" | md5sum 2>/dev/null | cut -d' ' -f1)
touch "/tmp/legion-work-${CWD_HASH}" 2>/dev/null
exit 0
