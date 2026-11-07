#!/bin/bash
# Test runner for turn-kind.sh (#1491): the turn-kind prompt (#1477) arrives
# with each prompt, as UserPromptSubmit additionalContext, before the agent
# answers -- not as a Stop continuation after it.
#
# Run from anywhere:
#   bash plugin/hooks/test-turn-kind.sh

set -u

# shellcheck source=tests/testutil.sh
source "$(dirname "${BASH_SOURCE[0]}")/tests/testutil.sh"

make_plugin_root turn-kind.sh

HOOK="$CLAUDE_PLUGIN_ROOT/hooks/turn-kind.sh"
CWD="/tmp/legion-test-1491"

# The turn-kind prompt, byte for byte as #1477's acceptance criterion gives it.
read -r -d '' TURN_KIND_PROMPT <<'PROMPT'
Before you end this turn, name what kind of turn it was.
**Design:** Sean, a peer, or you were working out what something should be. The deliverable is understanding. End with what you learned, what you still don't know, and the question that moves it forward. In a design turn the next move is a question, not an issue, a brief or a build. If you made one anyway, say so.
**Delivery:** you were carrying out a design that was already agreed. Finish it, or say plainly what blocks it.
**Can't tell:** treat it as design, and ask.
**A peer's ask:** a reply is owed, not a build. Answer it. A question back counts as an answer.
**If you acted on anything this turn:** name what you acted on that you assumed rather than found, so it can be checked.
PROMPT

echo "==> a prompt receives exactly the turn-kind guidance as UserPromptSubmit additionalContext"
out=$(echo "{\"cwd\":\"${CWD}\",\"session_id\":\"tk-1\",\"prompt\":\"hi\"}" | bash "$HOOK")
rc=$?
ctx=$(printf '%s' "$out" | jq -r '.hookSpecificOutput.additionalContext // empty')
event=$(printf '%s' "$out" | jq -r '.hookSpecificOutput.hookEventName // empty')
assert_eq "injected context is exactly the turn-kind prompt" "$ctx" "$TURN_KIND_PROMPT"
assert_eq "emitted for UserPromptSubmit" "$event" "UserPromptSubmit"
assert_not_contains "never a block decision" "$out" '"decision"'
assert_rc "exits 0" 0 "$rc"

echo "==> every prompt receives it, not once per session"
out=$(echo "{\"cwd\":\"${CWD}\",\"session_id\":\"tk-1\",\"prompt\":\"again\"}" | bash "$HOOK")
ctx=$(printf '%s' "$out" | jq -r '.hookSpecificOutput.additionalContext // empty')
assert_eq "a second prompt in the same session gets it again" "$ctx" "$TURN_KIND_PROMPT"

echo "==> the hook is registered on UserPromptSubmit and no longer emitted by stop.sh"
MANIFEST="$HOOKS_SRC_DIR/hooks.json"
registered=$(jq -r '.hooks.UserPromptSubmit[].hooks[].command' "$MANIFEST")
assert_contains "turn-kind.sh runs on UserPromptSubmit" "$registered" "hooks/turn-kind.sh"
assert_file_not_contains "stop.sh no longer carries the prompt" "$HOOKS_SRC_DIR/stop.sh" \
  "Before you end this turn, name what kind of turn it was."

finish_tests
