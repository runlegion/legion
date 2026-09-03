#!/bin/bash
# Test runner for the no-git-push PreToolUse hook (#827).
#
# Covers the three outcomes: REWRITE (translation is lossless), DENY (the
# command carries semantics `legion push` cannot express, so dropping them
# silently would run something the agent did not ask for), and PASS
# (not a git push, or the repo is not legion-covered).
#
#   bash plugin/hooks/test-no-git-push.sh

set -u

# shellcheck source=tests/testutil.sh
source "$(dirname "${BASH_SOURCE[0]}")/tests/testutil.sh"

make_plugin_root no-git-push.sh

export FAKE_WATCH="legion-test	/tmp/legion-test"

HOOK="$CLAUDE_PLUGIN_ROOT/hooks/no-git-push.sh"

run_hook() {
  local cmd="$1"
  # tool_name is load-bearing: the hook gates on it being Bash, matching
  # pre-bash-grep.sh. A real PreToolUse payload always carries it.
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

echo "==> rewrites plain pushes"
assert_rewritten "bare git push"            '"git push"'
assert_rewritten "push with remote"         '"git push origin"'
assert_rewritten "push remote + branch"     '"git push origin feat/x"'
assert_rewritten "push with -u"             '"git push -u origin feat/x"'
assert_rewritten "push with --set-upstream" '"git push --set-upstream origin feat/x"'

echo "==> resolves the binary by basename, like no-gh.sh"
assert_rewritten "absolute path git"   '"/opt/homebrew/bin/git push"'
assert_rewritten "leading whitespace"  '"   git push"'
assert_rewritten "global -C before subcommand" '"git -C /tmp/legion-test push"'
assert_rewritten "global --no-pager"   '"git --no-pager push"'

echo "==> carries an explicit branch into --branch"
assert_contains "branch threaded through" \
  "$(run_hook '"git push origin feat/x"')" '--branch feat/x'

echo "==> bare push omits --branch so legion push defaults to CWD"
assert_not_contains "no spurious --branch" \
  "$(run_hook '"git push"')" '--branch'

echo "==> denies what legion push cannot express"
assert_denied "force"             '"git push --force"'
assert_denied "force short"       '"git push -f origin feat/x"'
assert_denied "force-with-lease"  '"git push --force-with-lease"'
assert_denied "force-if-includes" '"git push --force-if-includes"'
assert_denied "delete"            '"git push --delete origin feat/x"'
assert_denied "delete short"      '"git push -d origin feat/x"'
assert_denied "tags"              '"git push --tags"'
assert_denied "mirror"            '"git push --mirror"'
assert_denied "prune"             '"git push --prune"'
assert_denied "all"               '"git push --all"'
assert_denied "refspec retarget"  '"git push origin feat/x:main"'

echo "==> deny names the offending flag rather than refusing generically"
assert_contains "names the flag" \
  "$(run_hook '"git push --force-with-lease"')" 'force-with-lease'

echo "==> a denied force push does NOT carry a rewrite"
assert_not_contains "force is never silently translated" \
  "$(run_hook '"git push --force"')" '"updatedInput"'

echo "==> passes through non-push git and non-git commands"
assert_passthrough "git status"    '"git status"'
assert_passthrough "git commit"    '"git commit -m x"'
assert_passthrough "git pushed-ish subcommand" '"git push-fake"'
assert_passthrough "not git at all" '"echo git push"'
assert_passthrough "legion push itself" '"legion push --repo legion"'

echo "==> announces the translation instead of rewriting silently"
assert_contains "additionalContext present" \
  "$(run_hook '"git push"')" '"additionalContext"'

echo "==> skips uncovered repos"
# Coverage is memoized per (session, repo), so clearing FAKE_WATCH after the
# covered cases above would still hit the warm cache entry. Use a cwd whose
# basename is absent from FAKE_WATCH instead -- a different repo, a different
# cache key, genuinely uncovered.
uncovered_out=$(printf '%s' '{"tool_name":"Bash","tool_input":{"command":"git push"},"cwd":"/tmp/not-a-legion-repo","session_id":"test"}' | bash "$HOOK")
assert_not_contains "repo not legion-covered (no rewrite)" "$uncovered_out" '"updatedInput"'
assert_not_contains "repo not legion-covered (no deny)" "$uncovered_out" '"permissionDecision": "deny"'

echo "==> honours the skip switch"
LEGION_SKIP_GIT_PUSH=1 assert_passthrough "LEGION_SKIP_GIT_PUSH=1" '"git push"'

echo "==> a rewrite PRESERVES the caller's other tool_input fields"
# Regression: emit_rewrite once emitted a bare {"command": ...}, and
# updatedInput replaces tool_input wholesale rather than merging -- so every
# other field the caller sent was silently dropped. A backgrounded push came
# back FOREGROUND and a raised timeout reverted to the default, while the
# rewrite itself looked correct. Measured before the fix: updatedInput was
# {"command":"legion push --repo legion"} and nothing else.
preserve_out=$(printf '%s' '{"tool_name":"Bash","tool_input":{"command":"git push","description":"push the branch","timeout":600000,"run_in_background":true},"cwd":"/tmp/legion-test","session_id":"test"}' | bash "$HOOK")
assert_contains "rewrite keeps description" "$preserve_out" '"description": "push the branch"'
assert_contains "rewrite keeps timeout" "$preserve_out" '"timeout": 600000'
assert_contains "rewrite keeps run_in_background" "$preserve_out" '"run_in_background": true'
assert_contains "rewrite still replaces command" "$preserve_out" '"command": "legion push'

echo "==> #883: composed commands never rewrite -- updatedInput replaces the WHOLE string"
assert_compound_deny() {
  local desc="$1" cmd="$2" out
  out=$(run_hook "$cmd")
  assert_contains "$desc (denied)" "$out" '"permissionDecision": "deny"'
  assert_not_contains "$desc (no updatedInput)" "$out" '"updatedInput"'
}
assert_compound_deny "pipe"           '"git push | tee /tmp/log"'
assert_compound_deny "and-chain"      '"git push && echo done"'
assert_compound_deny "or-chain"       '"git push || echo failed"'
assert_compound_deny "semicolon"      '"git push; git status"'
assert_compound_deny "redirect"       '"git push > out.txt"'
assert_compound_deny "input-redirect" '"git push < in.txt"'
# shellcheck disable=SC2016 # single-quoted on purpose: must reach the
# hook as literal text, never expand in this test's own shell.
assert_compound_deny "cmd-subst"      '"git push $(id)"'
# shellcheck disable=SC2016 # same: literal backtick, not command substitution.
assert_compound_deny "backtick"       '"git push `id`"'

echo "==> #883: a metacharacter glued directly onto push (no space) still denies, never passes through silently"
# read -a splits on whitespace only, so `push;`, `push&&`, `push$(id)` each
# tokenize as ONE word. Strict `[ "$SUBCOMMAND" = "push" ]` missed all of
# these entirely -- the command reached no branch of this hook, deny or
# rewrite, and a raw `git push` ran with no audit row. This is the specific
# regression: assert it denies, not that it merely avoids one bad rewrite.
assert_compound_deny "semicolon glued, no space"    '"git push;git status"'
assert_compound_deny "and-chain glued, no space"    '"git push&&echo done"'
# shellcheck disable=SC2016 # literal text, see above.
assert_compound_deny "cmd-subst glued, no space"    '"git push$(id)"'

echo "==> #883: composed commands never misread the next command's name as a branch"
# Measured before this guard existed: these two rewrote to
# `legion push --branch echo` / `... --branch tee`.
and_out=$(run_hook '"git push && echo done"')
assert_not_contains "no --branch echo" "$and_out" '--branch echo'
pipe_out=$(run_hook '"git push | tee /tmp/log"')
assert_not_contains "no --branch tee" "$pipe_out" '--branch tee'

echo "==> #883: the compound guard is scoped to push -- unrelated compound git commands still pass through"
assert_passthrough "compound but not a push" '"git status && echo hi"'

echo "==> #886: a leading unrelated command no longer hides push from the compound guard"
# Before #886, the subcommand walk only ever looked at the FIRST git
# invocation: `git status && git push` found SUBCOMMAND="status", matched
# neither the deny nor the rewrite path, and exited 0 -- a real
# `git push` ran with no audit row, no deny, nothing. The guard must
# fire regardless of what precedes `git push` in the chain, and
# regardless of whether the FIRST word is even `git` at all.
assert_compound_deny "leading unrelated git command"  '"git status && git push"'
assert_compound_deny "leading non-git command"        '"npm test && git push"'
assert_compound_deny "leading non-git, absolute path" '"npm test && /usr/bin/git push"'

echo "==> #886: still scoped to push -- a leading command chained to an unrelated git verb passes through"
assert_passthrough "leading command, no push anywhere" '"npm test && git status"'

echo "==> #915: a positional ref is resolved, not assumed to be a branch"
# The pre-#915 hook took POSITIONALS[1] as a branch unconditionally, so
# `git push origin v0.0.79` became `legion push --branch v0.0.79` and failed.
# That is what blocked rafters on a release tag, and it is why the release
# script shells out past this hook entirely. Resolution needs a real
# repository, so build one rather than asserting against the fixture cwd.
# The coverage gate (#353) derives the repo NAME from the cwd's basename and
# matches it against the watch list, so the fixture directory has to be named
# what the watch entry calls it -- not a random mktemp suffix.
REFPARENT="$(mktemp -d)"
REFREPO="${REFPARENT}/legion-test"
mkdir -p "$REFREPO"
git -C "$REFREPO" init --quiet
git -C "$REFREPO" config user.email t@t
git -C "$REFREPO" config user.name t
git -C "$REFREPO" commit --quiet --allow-empty -m init
git -C "$REFREPO" branch feat/real-branch
git -C "$REFREPO" tag v9.9.9
git -C "$REFREPO" branch ambiguous
git -C "$REFREPO" tag ambiguous

# The coverage gate (#353) keys on the watch list, so the temp repo has to be
# in it or the hook exits before resolving anything.
REAL_WATCH="$FAKE_WATCH"
export FAKE_WATCH=$'legion-test\t'"${REFREPO}"

run_hook_in() {
  local cwd="$1" cmd="$2"
  printf '%s' "{\"tool_name\":\"Bash\",\"tool_input\":{\"command\":${cmd}},\"cwd\":\"${cwd}\",\"session_id\":\"test\"}" | bash "$HOOK"
}

tag_out=$(run_hook_in "$REFREPO" '"git push origin v9.9.9"')
assert_contains "tag routes to --tag"              "$tag_out" '--tag v9.9.9'
assert_not_contains "tag never routes to --branch" "$tag_out" '--branch v9.9.9'

branch_out=$(run_hook_in "$REFREPO" '"git push origin feat/real-branch"')
assert_contains "branch still routes to --branch"  "$branch_out" '--branch feat/real-branch'
assert_not_contains "branch never routes to --tag" "$branch_out" '--tag feat/real-branch'

amb_out=$(run_hook_in "$REFREPO" '"git push origin ambiguous"')
assert_contains "branch+tag collision denies" "$amb_out" '"permissionDecision": "deny"'

missing_out=$(run_hook_in "$REFREPO" '"git push origin does-not-exist"')
assert_contains "unresolvable ref denies" "$missing_out" '"permissionDecision": "deny"'

# The bulk and compound refusals must survive the new resolution path.
bulk_out=$(run_hook_in "$REFREPO" '"git push --tags"')
assert_contains "--tags still denied after #915" "$bulk_out" '"permissionDecision": "deny"'
compound_out=$(run_hook_in "$REFREPO" '"git push origin v9.9.9 && echo done"')
assert_contains "compound tag push still denied" "$compound_out" '"permissionDecision": "deny"'

export FAKE_WATCH="$REAL_WATCH"
rm -rf "$REFPARENT"

# A repo the hook cannot inspect must behave exactly as it did before #915 --
# resolution is an improvement where available, never a new gate. A hook that
# blocks a legitimate push because it could not look at the repository is
# worse than the bug it was added to fix.
assert_rewritten "uninspectable cwd falls back to branch" '"git push origin feat/x"'


# --- #1117: a wrapper prefix reaches `git push` without any shell ----------
# --- metacharacter -----------------------------------------------------------

echo "==> #1117: wrapper prefixes deny, never rewrite"
assert_denied "env assignment"      '"env X=1 git push origin main"'
assert_denied "sudo"                '"sudo anything git push origin main"'
assert_denied "timeout"             '"timeout 5 git push origin main"'
assert_denied "bare VAR=val prefix" '"X=1 git push origin main"'
assert_denied "chained wrappers"    '"env X=1 timeout 5 git push origin main"'

echo "==> #1117: a wrapped push is never silently translated"
assert_not_contains "wrapped push carries no updatedInput" \
  "$(run_hook '"env X=1 git push origin main"')" '"updatedInput"'

echo "==> #1117: the wrapper deny names the wrapper class"
assert_contains "names wrapper class" \
  "$(run_hook '"env X=1 git push origin main"')" 'wrapper'

echo "==> #1117: a backslash-escaped git is detected the same as bare git"
assert_rewritten "escaped git still rewrites (no wrapper)" '"\\git push origin main"'

echo "==> #1117: an escaped git inside a compound chain still denies via the compound guard"
assert_denied "leading unrelated command, escaped git" '"npm test && \\git push origin main"'

echo "==> #1117: wrapper detection does not over-fire on unrelated commands"
assert_passthrough "env with no git anywhere" '"env X=1 npm test"'

finish_tests
