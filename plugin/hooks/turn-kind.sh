#!/bin/bash
# Legion UserPromptSubmit hook: the turn-kind prompt (#1477, moved here by
# #1491). Ask the agent to name what kind of turn it is -- design, delivery,
# can't tell, or a peer's ask -- when the prompt arrives, so the guidance
# shapes the answer instead of trailing it. It used to ride on Stop as
# additionalContext, which continued every turn and made the agent answer it
# in a second paragraph after its real reply.
#
# Fires on every prompt. Always exits 0 so a degraded legion never blocks a
# user prompt.

# shellcheck source=lib/emit.sh
source "${CLAUDE_PLUGIN_ROOT:-}/hooks/lib/emit.sh" 2>/dev/null || exit 0

# The prompt does not depend on the payload; drain it so the harness never
# writes into a closed pipe.
cat >/dev/null 2>&1 || true

read -r -d '' CTX <<'EOF'
Before you end this turn, name what kind of turn it was.
**Design:** Sean, a peer, or you were working out what something should be. The deliverable is understanding. End with what you learned, what you still don't know, and the question that moves it forward. In a design turn the next move is a question, not an issue, a brief or a build. If you made one anyway, say so.
**Delivery:** you were carrying out a design that was already agreed. Finish it, or say plainly what blocks it.
**Can't tell:** treat it as design, and ask.
**A peer's ask:** a reply is owed, not a build. Answer it. A question back counts as an answer.
**If you acted on anything this turn:** name what you acted on that you assumed rather than found, so it can be checked.
EOF

emit_context "UserPromptSubmit" "$CTX" 2>/dev/null || true
exit 0
