#!/bin/bash
# Test runner for the no-gh PreToolUse hook.
#
# Verifies the hook catches gh invocations regardless of how the binary
# is referenced: bare command, absolute path, leading whitespace. Run
# from anywhere:
#
#   bash plugin/hooks/test-no-gh.sh

set -u

# shellcheck source=tests/testutil.sh
source "$(dirname "${BASH_SOURCE[0]}")/tests/testutil.sh"

make_plugin_root no-gh.sh

# The hook gates on legion coverage; make the test repo covered via the
# stub's watch-list fixture.
export FAKE_WATCH="legion-test	/tmp/legion-test"

HOOK="$CLAUDE_PLUGIN_ROOT/hooks/no-gh.sh"

run_hook() {
  local cmd="$1"
  printf '%s' "{\"tool_input\":{\"command\":${cmd}},\"cwd\":\"/tmp/legion-test\",\"session_id\":\"test\"}" | bash "$HOOK"
}

assert_blocked() {
  local desc="$1" cmd="$2"
  assert_contains "$desc" "$(run_hook "$cmd")" '"permissionDecision": "deny"'
}

assert_allowed() {
  local desc="$1" cmd="$2"
  assert_not_contains "$desc" "$(run_hook "$cmd")" '"permissionDecision": "deny"'
}

assert_suggests() {
  local desc="$1" cmd="$2" needle="$3"
  assert_contains "$desc" "$(run_hook "$cmd")" "$needle"
}

# assert_rewrite DESC CMD EXPECTED_LEGION_CMD -- the hook must allow the
# call and replace it with exactly EXPECTED_LEGION_CMD via updatedInput.
assert_rewrite() {
  local desc="$1" cmd="$2" expected="$3" out
  out=$(run_hook "$cmd")
  assert_contains "$desc (allowed)" "$out" '"permissionDecision": "allow"'
  assert_contains "$desc (exact rewrite)" "$out" "\"command\": \"${expected}\""
}

# assert_deny_names_legion DESC CMD NEEDLE -- the hook must deny the call,
# never attach updatedInput, and its message must name NEEDLE (the legion
# equivalent it points at, or the flag it refuses to drop).
assert_deny_names_legion() {
  local desc="$1" cmd="$2" needle="$3" out
  out=$(run_hook "$cmd")
  assert_contains "$desc (denied)" "$out" '"permissionDecision": "deny"'
  assert_not_contains "$desc (no updatedInput)" "$out" '"updatedInput"'
  assert_contains "$desc (names $needle)" "$out" "$needle"
}

echo "==> blocks bare gh invocations (verbs with no legion rewrite)"
assert_blocked "bare gh subcommand"     '"gh pr merge 123"'
assert_blocked "bare gh with no args"   '"gh"'
assert_blocked "gh with leading space"  '"   gh pr merge 123"'

echo "==> blocks absolute-path gh invocations (verbs with no legion rewrite)"
assert_blocked "homebrew absolute path"  '"/opt/homebrew/bin/gh pr merge 123"'
assert_blocked "usr-local absolute path" '"/usr/local/bin/gh issue close 5"'
assert_blocked "tilde absolute path"     '"~/bin/gh pr merge 123"'
assert_blocked "absolute path no args"   '"/opt/homebrew/bin/gh"'

echo "==> absolute-path invocations still rewrite when the verb is eligible (#862)"
assert_rewrite "usr-bin absolute path rewrites" '"/usr/bin/gh pr view 1"' \
  'legion pr view --repo legion-test --number 1'

echo "==> allows commands that merely mention gh"
assert_allowed "ghostscript"      '"ghostscript --version"'
assert_allowed "git status"       '"git status"'
assert_allowed "echo gh"          '"echo gh pr merge"'
assert_allowed "grep gh logs"     '"grep gh /var/log/foo"'
assert_allowed "path with gh dir" '"ls /opt/ghosts/"'

echo "==> uncovered repo passes through"
out=$(printf '%s' '{"tool_input":{"command":"gh pr merge 1"},"cwd":"/tmp/uncovered-repo","session_id":"test"}' | bash "$HOOK")
assert_empty "gh allowed in a repo legion does not cover" "$out"


# --- #862: rewrite the lossless read subset ----------------------------------

echo "==> rewrites pr view / pr checks / pr list / issue view / issue list"
assert_rewrite "pr view"          '"gh pr view 42"'            'legion pr view --repo legion-test --number 42'
assert_rewrite "pr checks"        '"gh pr checks 42"'          'legion pr checks --repo legion-test --number 42'
assert_rewrite "pr list"          '"gh pr list"'                'legion pr list --repo legion-test'
assert_rewrite "issue view"       '"gh issue view 7"'          'legion issue view --repo legion-test --number 7'
assert_rewrite "issue list"       '"gh issue list"'             'legion issue list --repo legion-test'
assert_rewrite "flag-form number" '"gh pr view --number 99"'   'legion pr view --repo legion-test --number 99'

echo "==> rewrite responses announce the translation"
out_rw=$(run_hook '"gh pr view 42"')
assert_contains "rewrite carries additionalContext" "$out_rw" '"additionalContext"'
assert_contains "rewrite names the audit trail"      "$out_rw" 'legion audit'

echo "==> #828/#862: everything else stays denied with a named legion equivalent, never rewritten"
assert_deny_names_legion "pr merge"    '"gh pr merge 123"'   'legion pr merge --repo legion-test --number 123'
assert_deny_names_legion "pr close"    '"gh pr close 42"'    'legion pr close --repo legion-test --number 42'
assert_deny_names_legion "pr edit"     '"gh pr edit 42 --title x"' 'legion pr edit --repo legion-test --number 42'
assert_deny_names_legion "pr review"   '"gh pr review 42 --approve"' 'legion pr review --repo legion-test --number 42'
assert_deny_names_legion "pr comment -> legion comment" '"gh pr comment 42 --body x"' 'legion comment --repo legion-test --number 42'
assert_deny_names_legion "pr comments" '"gh pr comments 42"' 'legion pr comments --repo legion-test --number 42'
assert_deny_names_legion "issue create"  '"gh issue create --title x"'  'legion issue create --repo legion-test'
assert_deny_names_legion "issue close"   '"gh issue close 5"'   'legion issue close --repo legion-test --number 5'
assert_deny_names_legion "issue reopen"  '"gh issue reopen 5"'  'legion issue reopen --repo legion-test --number 5'
assert_deny_names_legion "issue edit"    '"gh issue edit 5 --title x"' 'legion issue edit --repo legion-test --number 5'
assert_deny_names_legion "issue comment" '"gh issue comment 5 --body x"' 'legion comment --repo legion-test --number 5'
assert_deny_names_legion "run list -> pr checks" '"gh run list"' 'legion pr checks --repo legion-test'
assert_deny_names_legion "run view -> pr checks" '"gh run view 1"' 'legion pr checks --repo legion-test'
assert_deny_names_legion "run watch -> pr checks" '"gh run watch 1"' 'legion pr checks --repo legion-test'

echo "==> pr diff stays denied: legion pr view has no diff content, so mapping is not lossless"
out_diff=$(run_hook '"gh pr diff 42"')
assert_contains "pr diff denied" "$out_diff" '"permissionDecision": "deny"'
assert_not_contains "pr diff not rewritten" "$out_diff" '"updatedInput"'
assert_contains "pr diff explains why" "$out_diff" 'never the diff content'
assert_contains "pr diff still names the closest legion command" "$out_diff" 'legion pr view --repo legion-test --number 42'

echo "==> lossy flags block the rewrite and name the flag, never silently drop it"
assert_deny_names_legion "pr view --json"       '"gh pr view 42 --json title"'  '--json'
assert_deny_names_legion "pr view --json= form" '"gh pr view 42 --json=title"'  '--json'
assert_deny_names_legion "pr view -c/--comments" '"gh pr view 42 -c"'          '-c'
assert_deny_names_legion "pr view -R cross-repo" '"gh pr view 42 -R owner/repo"' '-R'
assert_deny_names_legion "pr view -w/--web"      '"gh pr view 42 -w"'          '-w'
assert_deny_names_legion "pr checks --watch"     '"gh pr checks 42 --watch"'   '--watch'
assert_deny_names_legion "pr checks --required"  '"gh pr checks 42 --required"' '--required'
assert_deny_names_legion "pr list any flag"      '"gh pr list --state closed"' '--state'
assert_deny_names_legion "issue view --jq"       '"gh issue view 7 --jq .title"' '--jq'
assert_deny_names_legion "issue list any flag"   '"gh issue list --label bug"' '--label'

echo "==> missing number falls through to the placeholder deny, never a broken rewrite"
assert_suggests "placeholder when absent" '"gh pr view"' '--number <n>'
out_noview=$(run_hook '"gh pr view"')
assert_not_contains "no rewrite without a number" "$out_noview" '"updatedInput"'
assert_contains "still denied without a number" "$out_noview" '"permissionDecision": "deny"'

echo "==> compound commands never rewrite -- updatedInput replaces the WHOLE string"
assert_compound_deny() {
  local desc="$1" cmd="$2" out
  out=$(run_hook "$cmd")
  assert_contains "$desc (denied)" "$out" '"permissionDecision": "deny"'
  assert_not_contains "$desc (no updatedInput)" "$out" '"updatedInput"'
}
assert_compound_deny "pipe"       '"gh pr view 42 | jq .title"'
assert_compound_deny "redirect"   '"gh pr view 42 > out.txt"'
assert_compound_deny "and-chain"  '"gh pr view 42 && echo done"'
# shellcheck disable=SC2016 # single-quoted on purpose: must reach the
# hook as literal text, never expand in this test's own shell.
assert_compound_deny "cmd-subst"  '"gh pr view 42 $(id)"'
# shellcheck disable=SC2016 # same: literal backtick, not command substitution.
assert_compound_deny "backtick"   '"gh pr list `id`"'

echo "==> #886: a leading unrelated command no longer hides gh from the compound guard"
# Before #886, this hook only ever looked at TOKENS[0]: `echo hi && gh pr
# merge 123` had FIRST_BIN="echo", so the whole command passed through
# with no deny, no rewrite -- a real `gh` call ran unaudited. The guard
# must fire regardless of what precedes `gh` in the chain.
assert_compound_deny "leading unrelated command" '"echo hi && gh pr merge 123"'
assert_compound_deny "leading unrelated, rewrite-eligible verb" '"echo hi && gh pr view 42"'
assert_compound_deny "leading unrelated, absolute path gh" '"echo hi && /usr/bin/gh pr merge 123"'

echo "==> #886: compound commands that never mention gh still pass through untouched"
assert_empty "compound but no gh anywhere" "$(run_hook '"echo hi && ls /tmp"')"


# --- #828: exact redirect instead of a fixed menu, still holds -------------

echo "==> reads the number from the --number flag form too"
assert_suggests "flag-form number is used" '"gh pr view --number 99"' '--number 99'

echo "==> unmapped shapes point at group help and invent nothing"
assert_suggests "gh api falls back"   '"gh api /repos/x/y"' 'legion --help'
assert_suggests "unknown pr verb"     '"gh pr sync"'        'legion pr --help'
out_api=$(run_hook '"gh api /repos/x/y"')
assert_not_contains "no fabricated translation for gh api" "$out_api" 'legion api'

echo "==> the old fixed menu is gone"
out_merge=$(run_hook '"gh pr merge 42"')
assert_not_contains "does not print the 8-verb catalog" "$out_merge" 'legion pr review --repo <name>'

finish_tests
