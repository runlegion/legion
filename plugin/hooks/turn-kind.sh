#!/bin/bash
# Legion UserPromptSubmit hook: one line of guidance with every prompt
# (#1497). It replaced the six-line turn-kind prompt (#1477, moved to prompt
# time by #1491), whose opening line read as an output instruction and put
# a turn-kind footer on replies.
#
# Fires on every prompt. Always exits 0 so a degraded legion never blocks a
# user prompt.

# shellcheck source=lib/emit.sh
source "${CLAUDE_PLUGIN_ROOT:-}/hooks/lib/emit.sh" 2>/dev/null || exit 0

# The prompt does not depend on the payload; drain it so the harness never
# writes into a closed pipe.
cat >/dev/null 2>&1 || true

CTX="Nothing to act on here unless you learn something new. If you do, reflect it."

emit_context "UserPromptSubmit" "$CTX" 2>/dev/null || true
exit 0
