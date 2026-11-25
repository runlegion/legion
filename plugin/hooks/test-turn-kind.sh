#!/bin/bash
# Test runner for turn-kind.sh (#1497): each prompt carries one line of
# guidance, as UserPromptSubmit additionalContext, and no hook sends the
# turn-kind text (#1477) any more.
#
# Run from anywhere:
#   bash plugin/hooks/test-turn-kind.sh

set -u

# shellcheck source=tests/testutil.sh
source "$(dirname "${BASH_SOURCE[0]}")/tests/testutil.sh"

make_plugin_root turn-kind.sh

HOOK="$CLAUDE_PLUGIN_ROOT/hooks/turn-kind.sh"
CWD="/tmp/legion-test-1497"

GUIDANCE="Nothing to act on here unless you learn something new. If you do, reflect it."

echo "==> a prompt receives exactly the one-line guidance as UserPromptSubmit additionalContext"
out=$(echo "{\"cwd\":\"${CWD}\",\"session_id\":\"tk-1\",\"prompt\":\"hi\"}" | bash "$HOOK")
rc=$?
ctx=$(printf '%s' "$out" | jq -r '.hookSpecificOutput.additionalContext // empty')
event=$(printf '%s' "$out" | jq -r '.hookSpecificOutput.hookEventName // empty')
assert_eq "injected context is exactly the guidance line" "$ctx" "$GUIDANCE"
assert_eq "emitted for UserPromptSubmit" "$event" "UserPromptSubmit"
assert_not_contains "never a block decision" "$out" '"decision"'
assert_rc "exits 0" 0 "$rc"

echo "==> every prompt receives it, not once per session"
out=$(echo "{\"cwd\":\"${CWD}\",\"session_id\":\"tk-1\",\"prompt\":\"again\"}" | bash "$HOOK")
ctx=$(printf '%s' "$out" | jq -r '.hookSpecificOutput.additionalContext // empty')
assert_eq "a second prompt in the same session gets it again" "$ctx" "$GUIDANCE"

echo "==> the hook is registered on UserPromptSubmit"
MANIFEST="$HOOKS_SRC_DIR/hooks.json"
registered=$(jq -r '.hooks.UserPromptSubmit[].hooks[].command' "$MANIFEST")
assert_contains "turn-kind.sh runs on UserPromptSubmit" "$registered" "hooks/turn-kind.sh"

echo "==> no hook sends the turn-kind text"
for script in "$HOOKS_SRC_DIR"/*.sh "$HOOKS_SRC_DIR"/lib/*.sh; do
  case "$(basename "$script")" in test-*) continue ;; esac
  name="lib/$(basename "$script")"
  [ "$(dirname "$script")" = "$HOOKS_SRC_DIR" ] && name="$(basename "$script")"
  for marker in "name what kind of turn it was" "Design:" "Delivery:" \
    "Can't tell:" "A peer's ask:" "If you acted on anything this turn"; do
    assert_file_not_contains "$name does not carry '$marker'" "$script" "$marker"
  done
done

finish_tests
