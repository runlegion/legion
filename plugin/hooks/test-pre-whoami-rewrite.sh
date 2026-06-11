#!/bin/bash
# Test runner for the pre-whoami-rewrite PreToolUse hook.
#
# Uses the shared stub legion (FAKE_WHOAMI_BODY controls whether an
# identity "exists") to exercise the block + bypass paths. Run from
# anywhere:
#
#   bash plugin/hooks/test-pre-whoami-rewrite.sh

set -u

# shellcheck source=tests/testutil.sh
source "$(dirname "${BASH_SOURCE[0]}")/tests/testutil.sh"

make_plugin_root pre-whoami-rewrite.sh

IDENTITY_BODY='- I am the test agent. I do test things.

DECISION ORDER: do the right thing.'

HOOK="$CLAUDE_PLUGIN_ROOT/hooks/pre-whoami-rewrite.sh"

run_hook() {
  local cmd="$1"
  printf '%s' "{\"tool_input\":{\"command\":${cmd}},\"cwd\":\"/tmp/test\",\"session_id\":\"t\"}" | bash "$HOOK"
}

assert_blocked() {
  local desc="$1" cmd="$2"
  local out
  out=$(FAKE_WHOAMI_BODY="$IDENTITY_BODY" run_hook "$cmd")
  assert_contains "$desc" "$out" '"permissionDecision": "deny"'
}

# assert_allowed DESC CMD [BODY] -- pass an empty BODY to simulate a repo
# with no existing identity.
assert_allowed() {
  local desc="$1" cmd="$2" body="${3-$IDENTITY_BODY}"
  local out
  out=$(FAKE_WHOAMI_BODY="$body" run_hook "$cmd")
  assert_not_contains "$desc" "$out" '"permissionDecision": "deny"'
}

echo "==> blocks whoami rewrite when identity already exists"
assert_blocked "bare --whoami rewrite" \
  '"legion reflect --whoami --repo test --text \"new id\""'
assert_blocked "absolute-path legion" \
  '"/opt/homebrew/bin/legion reflect --whoami --repo test --text \"new id\""'
assert_blocked "--domain identity equivalent" \
  '"legion reflect --domain identity --repo test --text \"new id\""'

echo "==> allows when --force is set"
assert_allowed "--force bypass" \
  '"legion reflect --whoami --force --repo test --text \"new id\""'

echo "==> allows when --follows is set"
assert_allowed "--follows bypass" \
  '"legion reflect --whoami --follows 019abc --repo test --text \"new beat\""'

echo "==> allows first-time identity (no existing whoami)"
assert_allowed "first-time setup" \
  '"legion reflect --whoami --repo test --text \"initial id\""' \
  ""

echo "==> ignores non-reflect legion commands"
assert_allowed "recall is not reflect" \
  '"legion recall --repo test --context foo"'
assert_allowed "kanban is not reflect" \
  '"legion kanban list --repo test"'

echo "==> ignores reflect without identity domain"
assert_allowed "regular reflect with no domain" \
  '"legion reflect --repo test --text \"a project lesson\""'
assert_allowed "reflect with non-identity domain" \
  '"legion reflect --domain checkpoint --repo test --text \"x\""'

echo "==> ignores commands not starting with legion"
assert_allowed "git command" \
  '"git status"'
assert_allowed "echo mentions legion reflect --whoami" \
  '"echo legion reflect --whoami --repo test"'

finish_tests
