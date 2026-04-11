#!/bin/bash
# Shared helper for legion hooks. Tracks failed legion invocations and
# renders a visible warning block so agents notice when legion is degraded
# instead of silently losing context.
#
# Usage from a hook script:
#
#   source "${CLAUDE_PLUGIN_ROOT}/hooks/_legion-warn.sh"
#
#   SOME_OUTPUT=$("$LEGION" recall --repo "$REPO" ... 2>>"$LOG")
#   legion_check $? "recall"
#
#   # ... later, when building additionalContext:
#   WARN=$(legion_warnings_block)
#   if [ -n "$WARN" ]; then
#     OUTPUT="${WARN}"$'\n\n'"${OUTPUT}"
#   fi
#
# The helper never exits the calling script and never writes to stderr on
# its own -- it only accumulates state and renders a string on request.

# Accumulator for failed actions (one per line, "- <action>").
# Hooks source this file fresh each invocation, so the empty init is safe.
LEGION_WARNINGS=""

# Record a failure if the exit code is nonzero.
#   $1 -- exit code from the most recent command (pass $? immediately after it)
#   $2 -- short action name ("recall", "sync", "bullpen", etc)
legion_check() {
  if [ "${1:-0}" -ne 0 ]; then
    LEGION_WARNINGS="${LEGION_WARNINGS}- ${2} failed (exit ${1})"$'\n'
  fi
}

# Render the warning block. Empty string if no failures were recorded.
# The sentinel "[Legion WARNING]" is load-bearing -- integration tests
# grep for it to confirm visible degradation surfacing.
legion_warnings_block() {
  if [ -z "$LEGION_WARNINGS" ]; then
    return 0
  fi
  printf '%s' "[Legion WARNING] Legion is degraded. One or more hook commands failed:
${LEGION_WARNINGS}Details: /tmp/legion-hook-errors.log
Common causes: missing binary at \${CLAUDE_PLUGIN_ROOT}/bin/legion, DB schema mismatch, corrupted embedding column (TEXT instead of BLOB), data_dir split-brain between ~/Library/Application Support/legion and ~/.claude/plugins/data/legion-legion, wrong CLAUDE_PLUGIN_ROOT in hook subshell.
Recall and bullpen context is missing from this session until legion recovers."
}
