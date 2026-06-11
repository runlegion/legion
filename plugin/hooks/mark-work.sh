#!/bin/bash
# Legion PostToolUse hook: mark this session as having done real work so the
# Stop hook fires its reflect prompt only when there is something to reflect
# on. Prose-only Q&A sessions (no tool calls) skip the prompt.
#
# Touches /tmp/legion-work-<md5(cwd)>. The Stop hook reads this marker.

# shellcheck source=lib/prelude.sh
source "${CLAUDE_PLUGIN_ROOT:-}/hooks/lib/prelude.sh" 2>/dev/null || exit 0

legion_hook_parse || exit 0

if [ -z "$CWD" ]; then
  exit 0
fi

CWD_HASH=$(legion_hash_str "$CWD")
touch "/tmp/legion-work-${CWD_HASH}" 2>/dev/null
exit 0
