#!/bin/bash
# Test runner for the no-git-commit PreToolUse hook (#856).
#
# Covers the three outcomes: REWRITE (translation is lossless), DENY (the
# command carries semantics `legion commit` cannot express, so dropping
# them silently would run something the agent did not ask for), and PASS
# (not a git commit, or the repo is not legion-covered).
#
#   bash plugin/hooks/test-no-git-commit.sh

set -u

# shellcheck source=tests/testutil.sh
source "$(dirname "${BASH_SOURCE[0]}")/tests/testutil.sh"

make_plugin_root no-git-commit.sh

export FAKE_WATCH="legion-test	/tmp/legion-test"

HOOK="$CLAUDE_PLUGIN_ROOT/hooks/no-git-commit.sh"

run_hook() {
  local cmd="$1"
  # tool_name is load-bearing: the hook gates on it being Bash, matching
  # no-git-push.sh. A real PreToolUse payload always carries it.
  printf '%s' "{\"tool_name\":\"Bash\",\"tool_input\":{\"command\":${cmd}},\"cwd\":\"/tmp/legion-test\",\"session_id\":\"test\"}" | bash "$HOOK"
}

assert_rewritten() {
  local desc="$1" cmd="$2"
  assert_contains "$desc" "$(run_hook "$cmd")" '"updatedInput"'
}

assert_denied() {
  local desc="$1" cmd="$2"
  assert_contains "$desc" "$(run_hook "$cmd")" '"permissionDecision": "deny"'
}

assert_passthrough() {
  local desc="$1" cmd="$2"
  local out
  out="$(run_hook "$cmd")"
  assert_not_contains "$desc (no rewrite)" "$out" '"updatedInput"'
  assert_not_contains "$desc (no deny)" "$out" '"permissionDecision": "deny"'
}

# extract_message_file CMD_JSON -- run the hook on CMD_JSON, pull the
# --message-file path out of the rewritten command, and echo the file's
# contents. Used to prove the tempfile round-trips the exact message bytes
# rather than a re-quoted/re-escaped approximation.
extract_message_file() {
  local out cmd file
  out="$(run_hook "$1")"
  cmd=$(printf '%s' "$out" | jq -r '.hookSpecificOutput.updatedInput.command // empty')
  file=$(printf '%s' "$cmd" | sed -n "s/.*--message-file //p" | sed "s/^.//;s/.\$//")
  [ -n "$file" ] && [ -f "$file" ] && cat "$file"
}

echo "==> rewrites a plain -m commit"
assert_rewritten "bare -m"            '"git commit -m fix"'
assert_rewritten "quoted -m"          '"git commit -m \"feat(#856): thing\""'
SINGLE_QUOTED_CMD=$(printf "git commit -m 'feat(#856): thing'" | jq -Rs .)
assert_rewritten "single-quoted -m"   "$SINGLE_QUOTED_CMD"
assert_rewritten "--message long form" '"git commit --message \"feat(#856): thing\""'
assert_rewritten "-F short form"       '"git commit -F /tmp/msg.txt"'
assert_rewritten "--file long form"    '"git commit --file /tmp/msg.txt"'

echo "==> resolves the binary by basename and global options, like no-git-push.sh"
assert_rewritten "absolute path git"   '"/opt/homebrew/bin/git commit -m fix"'
assert_rewritten "leading whitespace"  '"   git commit -m fix"'
assert_rewritten "global --no-pager"   '"git --no-pager commit -m fix"'

echo "==> the rewritten command carries --repo and --message-file, never a re-quoted --message"
CMD_OUT=$(run_hook '"git commit -m fix"')
assert_contains "carries --repo" "$CMD_OUT" "--repo"
assert_contains "carries --message-file" "$CMD_OUT" "--message-file"
assert_not_contains "never re-quotes as --message" "$CMD_OUT" "--message '"

echo "==> the message tempfile carries the exact bytes, embedded newlines included"
# Build the JSON payload with a real embedded newline the way jq would emit it.
MULTILINE_CMD=$(printf 'git commit -m "feat(#856): thing\n\nCo-Authored-By: X <x@y.com>"' | jq -Rs .)
CONTENT=$(extract_message_file "$MULTILINE_CMD")
assert_contains "subject line preserved" "$CONTENT" "feat(#856): thing"
assert_contains "trailer preserved" "$CONTENT" "Co-Authored-By: X <x@y.com>"
BLANK_LINE_COUNT=$(printf '%s\n' "$CONTENT" | sed -n '2p')
assert_eq "blank line between subject and trailer preserved" "$BLANK_LINE_COUNT" ""

echo "==> -C retargets the checkout AND the resolved --repo"
CPATH_OUT=$(run_hook '"git -C /tmp/legion-test commit -m fix"')
assert_contains "prefixes cd to the -C path" "$CPATH_OUT" "cd '/tmp/legion-test'"
assert_contains "resolves --repo from the -C path basename" "$CPATH_OUT" "--repo 'legion-test'"

echo "==> LEGION_REPO still wins over a -C path, per lib/prelude.sh precedence"
LEGION_REPO_OUT=$(LEGION_REPO=legion-test run_hook '"git -C /tmp/legion-test commit -m fix"')
assert_contains "LEGION_REPO wins" "$LEGION_REPO_OUT" "--repo 'legion-test'"

echo "==> denies -a/--all and its bundled forms, telling the agent to git add first"
assert_denied "-a"           '"git commit -a -m fix"'
assert_denied "--all"        '"git commit --all -m fix"'
assert_denied "-am bundle"   '"git commit -am fix"'
assert_denied "-ma bundle"   '"git commit -ma fix"'
GIT_ADD_OUT=$(run_hook '"git commit -am fix"')
assert_contains "names git add as the remedy" "$GIT_ADD_OUT" 'git add'

echo "==> denies --amend outright, no legion equivalent"
assert_denied "--amend"        '"git commit --amend"'
assert_denied "--amend -m msg" '"git commit --amend -m fix"'

echo "==> denies -n/--no-verify"
assert_denied "-n"           '"git commit -n -m fix"'
assert_denied "--no-verify"  '"git commit --no-verify -m fix"'

echo "==> denies --allow-empty-message"
assert_denied "--allow-empty-message" '"git commit --allow-empty-message -m fix"'

echo "==> denies commit-reuse -C/-c (as commit flags, not the global -C)"
assert_denied "-C <commit-ish>" '"git commit -C HEAD~1"'
assert_denied "-c <commit-ish>" '"git commit -c HEAD~1"'

echo "==> denies -e/--edit/--interactive/--patch"
assert_denied "-e"            '"git commit -e -m fix"'
assert_denied "--edit"        '"git commit --edit -m fix"'
assert_denied "--interactive" '"git commit --interactive"'
assert_denied "--patch"       '"git commit --patch"'

echo "==> denies a bare commit with no message, suggesting the -m form"
assert_denied "bare commit" '"git commit"'
BARE_OUT=$(run_hook '"git commit"')
assert_contains "suggests --message" "$BARE_OUT" 'legion commit'

echo "==> denies what this translator cannot confidently parse"
assert_denied "unterminated quote"    '"git commit -m \"unterminated"'
assert_denied "trailing content"      '"git commit -m fix extra"'
assert_denied "double -m"             '"git commit -m fix -m second"'
assert_denied "inline --message="     '"git commit --message=fix"'
assert_denied "inline --file="        '"git commit --file=/tmp/msg.txt"'
assert_denied "unrecognized flag"     '"git commit --squash=HEAD"'
assert_denied "unrecognized short flag with attached value" '"git commit -Cmain"'
assert_denied "-m with no value"      '"git commit -m"'
assert_denied "positional pathspec"   '"git commit -m fix extra.txt"'

echo "==> denies live shell metacharacters this hook cannot evaluate on the ORIGINAL command's behalf"
# Command substitution and glued shell operators inside the message value
# were supposed to be evaluated when the ORIGINAL command ran. Capturing
# them as inert literal text silently defeats that -- the substitution
# never runs, and the raw syntax (or a truncated command) lands in history
# instead. Deny is the only honest outcome: there is no lossless rewrite.
# shellcheck disable=SC2016  # single-quoted ON PURPOSE: these are the
# literal shell-substitution bytes under test. If they expanded here the
# fixture would deliver already-evaluated text and the assertion would
# prove nothing -- the exact vacuous-fixture trap that let the truncated
# grep pattern pass its own test.
# shellcheck disable=SC2016 # single-quoted printf format strings, on
# purpose: these must reach jq as literal source text, never expand in
# this test's own shell.
HEREDOC_CMD=$(printf 'git commit -m "$(cat <<'"'"'EOF'"'"'\nfix(x): real subject\n\nCo-Authored-By: Someone <a@b.c>\nEOF\n)"' | jq -Rs .)
assert_denied "heredoc-in-command-substitution (repro 1)" "$HEREDOC_CMD"
# shellcheck disable=SC2016 # same: literal text, not expansion.
DATE_SUBST_CMD=$(printf 'git commit -m "fix(#123): $(date +%%F) rollout"' | jq -Rs .)
assert_denied "command substitution mid-message (repro 2)" "$DATE_SUBST_CMD"
assert_denied "glued && absorbs a second command (repro 3a)" '"git commit -m done&&true"'
assert_denied "glued ; absorbs a second command (repro 3b)" '"git commit -m done;true"'
# shellcheck disable=SC2016 # same: literal text, not expansion.
BACKTICK_CMD=$(printf 'git commit -m done`id`' | jq -Rs .)
assert_denied "glued backtick command substitution (repro 3c)" "$BACKTICK_CMD"
assert_denied "bare pipe"  '"git commit -m done|cat"'
# shellcheck disable=SC2016 # same: literal text, not expansion.
DOLLAR_VAR_CMD=$(printf 'git commit -m "release $HOME build"' | jq -Rs .)
assert_denied "bare \$VAR expansion inside double quotes (same class as \$(...))" "$DOLLAR_VAR_CMD"

echo "==> but does NOT over-deny: single quotes are inert, escaped \$/\` are literal"
# shellcheck disable=SC2016 # same: literal text, not expansion.
SINGLE_QUOTED_SUBST_CMD=$(printf "git commit -m '\$(date) is not evaluated in single quotes'" | jq -Rs .)
assert_rewritten "single-quoted \$(...) stays inert -- safe to rewrite" "$SINGLE_QUOTED_SUBST_CMD"
SQ_CONTENT=$(extract_message_file "$SINGLE_QUOTED_SUBST_CMD")
# shellcheck disable=SC2016 # matching against literal text, not expansion.
assert_contains "single-quoted literal text preserved verbatim" "$SQ_CONTENT" '$(date) is not evaluated'
# shellcheck disable=SC2016 # same: literal text, not expansion.
ESCAPED_DOLLAR_CMD=$(printf 'git commit -m "price is \\$5"' | jq -Rs .)
assert_rewritten "backslash-escaped \\\$ inside double quotes is literal, not live" "$ESCAPED_DOLLAR_CMD"
ESC_CONTENT=$(extract_message_file "$ESCAPED_DOLLAR_CMD")
# shellcheck disable=SC2016 # matching against literal text, not expansion.
assert_contains "escaped dollar preserved as a literal character" "$ESC_CONTENT" 'price is $5'

echo "==> a denied commit does NOT carry a rewrite"
assert_not_contains "amend is never silently translated" \
  "$(run_hook '"git commit --amend"')" '"updatedInput"'

echo "==> passes through non-commit git and non-git commands"
assert_passthrough "git status"              '"git status"'
assert_passthrough "git push"                '"git push"'
assert_passthrough "git commit-ish subcommand" '"git commit-fake -m fix"'
assert_passthrough "not git at all"          '"echo git commit -m fix"'
assert_passthrough "legion commit itself"    '"legion commit --repo legion --message fix"'

echo "==> announces the translation instead of rewriting silently"
assert_contains "additionalContext present" \
  "$(run_hook '"git commit -m fix"')" '"additionalContext"'

echo "==> skips uncovered repos"
# Coverage is memoized per (session, repo), so clearing FAKE_WATCH after the
# covered cases above would still hit the warm cache entry. Use a cwd/repo
# whose basename is absent from FAKE_WATCH instead -- a different repo, a
# different cache key, genuinely uncovered.
uncovered_out=$(printf '%s' '{"tool_name":"Bash","tool_input":{"command":"git commit -m fix"},"cwd":"/tmp/not-a-legion-repo","session_id":"test"}' | bash "$HOOK")
assert_not_contains "repo not legion-covered (no rewrite)" "$uncovered_out" '"updatedInput"'
assert_not_contains "repo not legion-covered (no deny)" "$uncovered_out" '"permissionDecision": "deny"'

echo "==> the -C path's own repo drives coverage, not the session's"
uncovered_c_out=$(run_hook '"git -C /tmp/not-a-legion-repo commit -m fix"')
assert_not_contains "-C target uncovered (no rewrite)" "$uncovered_c_out" '"updatedInput"'
assert_not_contains "-C target uncovered (no deny)" "$uncovered_c_out" '"permissionDecision": "deny"'

echo "==> honours the skip switch"
LEGION_SKIP_GIT_COMMIT=1 assert_passthrough "LEGION_SKIP_GIT_COMMIT=1" '"git commit -m fix"'

echo "==> #886: a leading unrelated command no longer hides commit from the compound guard"
# Before #886, the subcommand walk only ever looked at the FIRST git
# invocation: `git add -A && git commit -m x` found SUBCOMMAND="add",
# matched neither the deny nor the rewrite path, and exited 0 -- a real
# `git commit` ran with no audit row, no message validation, no signer
# preflight, nothing. The guard must fire regardless of what precedes
# `git commit` in the chain, and regardless of whether the first word is
# even `git` at all -- `git add -A && git commit -m "..." && git push`
# is the standard shape an agent lands work with.
assert_denied "leading unrelated git command"  '"git add -A && git commit -m fix"'
assert_denied "leading non-git command"        '"npm test && git commit -m fix"'
assert_denied "full land-work chain"           '"git add -A && git commit -m fix && git push"'
LAND_WORK_OUT=$(run_hook '"git add -A && git commit -m fix && git push"')
assert_not_contains "land-work chain not silently rewritten" "$LAND_WORK_OUT" '"updatedInput"'

echo "==> #886: still scoped to commit -- a leading command chained to an unrelated git verb passes through"
assert_passthrough "leading command, no commit anywhere" '"npm test && git status"'

echo "==> #886: a genuinely safe message is NOT over-denied by the compound guard"
# The compound fallback above only fires when `commit` is not the FIRST
# subcommand -- confirmed here: a normal commit whose single-quoted
# message happens to contain metacharacters (already covered above) and
# a normal commit with no composition at all must still rewrite cleanly.
assert_rewritten "plain commit still rewrites, unaffected by the #886 guard" '"git commit -m fix"'

finish_tests
