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

# Render the warning block. Always empty -- per #383, the agent-facing
# block was reading as instruction ("recall is degraded, stop relying on it")
# and shaping behavior away from legion for non-legion failures (upstream
# channel-delivery flakiness). Failures still flow to /tmp/legion-hook-errors.log
# via each hook's stderr redirect, which is the diagnostic path operators want.
# Callers retain the legion_warnings_block / legion_check API surface so hooks
# do not need to be edited individually; the function is now a no-op.
legion_warnings_block() {
  return 0
}
