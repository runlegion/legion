#!/bin/bash
# Test runner for pre-bash-grep.sh and the _legion-prequery.sh helpers.
#
# Uses the shared fake plugin root + parameterized stub legion from
# tests/testutil.sh. Each test feeds synthetic hook JSON over stdin and
# asserts on stdout shape. Run from anywhere:
#
#   bash plugin/hooks/test-pre-bash-grep.sh
#
# Exits 0 on success, 1 on any failed assertion.

set -u

# shellcheck source=tests/testutil.sh
source "$(dirname "${BASH_SOURCE[0]}")/tests/testutil.sh"

make_plugin_root pre-bash-grep.sh

# Fixtures: repo "legion" is covered (watch + stats) and indexed; Symbol /
# fn_main / main resolve to local-repo sym hits, commonword / dictword to
# cross-repo hits the #458 relevance gate must filter out.
export FAKE_WATCH="legion	/tmp/legion"
export FAKE_STATS="legion:5"
export FAKE_INDEX_JSON='[{"repo":"legion","lang":"rust","size_bytes":100,"updated_at":"2026-01-01T00:00:00Z"}]'
export FAKE_SYM_LOCAL="Symbol fn_main main"
export FAKE_SYM_LOCAL_REPO="legion"
export FAKE_SYM_REMOTE="commonword dictword"
export FAKE_SYM_REMOTE_REPO="huttspawn"

HOOK="$CLAUDE_PLUGIN_ROOT/hooks/pre-bash-grep.sh"

# ---------- _legion-prequery.sh helper unit tests ----------

# shellcheck source=_legion-prequery.sh
source "$CLAUDE_PLUGIN_ROOT/hooks/_legion-prequery.sh"

echo "==> bash-binary detection"
assert_eq "leading grep -> grep"  "$(legion_prequery_bash_binary 'grep -r foo .')"   "grep"
assert_eq "leading rg -> rg"      "$(legion_prequery_bash_binary 'rg --no-heading x')" "rg"
assert_eq "leading find -> find"  "$(legion_prequery_bash_binary 'find . -name foo')" "find"
assert_eq "leading fd -> fd"      "$(legion_prequery_bash_binary 'fd Symbol src/')"   "fd"
assert_eq "leading ls -> empty"   "$(legion_prequery_bash_binary 'ls -la')"           ""
assert_eq "echo -> empty"         "$(legion_prequery_bash_binary 'echo grep is not running')" ""

echo "==> pattern extraction"
assert_eq "grep -r PAT ."            "$(legion_prequery_extract_pattern 'grep -r Symbol .' grep)" "Symbol"
assert_eq "grep --no-heading PAT ."  "$(legion_prequery_extract_pattern 'grep --no-heading SymbolName src/' grep)" "SymbolName"
assert_eq "grep -e PAT ."            "$(legion_prequery_extract_pattern 'grep -e MyFunc src/' grep)" "MyFunc"
assert_eq "grep --regexp=PAT ."      "$(legion_prequery_extract_pattern 'grep --regexp=Foo src/' grep)" "Foo"
assert_eq "rg PAT"                   "$(legion_prequery_extract_pattern 'rg fn_main src/' rg)" "fn_main"
assert_eq "find . -name PAT"         "$(legion_prequery_extract_pattern 'find . -name Symbol' find)" "Symbol"
assert_eq "find skips path arg"      "$(legion_prequery_extract_pattern 'find ./src -name Foo' find)" "Foo"

echo "==> symbol-shape detection"
legion_prequery_is_symbol "Symbol" && actual=0 || actual=1
assert_eq "CamelCase symbol"  "$actual" "0"
legion_prequery_is_symbol "fn_main" && actual=0 || actual=1
assert_eq "snake_case symbol" "$actual" "0"
legion_prequery_is_symbol "ab" && actual=0 || actual=1
assert_eq "too-short rejected" "$actual" "1"
legion_prequery_is_symbol "foo|bar" && actual=0 || actual=1
assert_eq "regex alternation rejected" "$actual" "1"
legion_prequery_is_symbol "two words" && actual=0 || actual=1
assert_eq "whitespace rejected" "$actual" "1"
legion_prequery_is_symbol "^anchored" && actual=0 || actual=1
assert_eq "anchored rejected" "$actual" "1"

echo "==> bypass reason detection"
LEGION_BYPASS_GREP=1 reason=$(legion_prequery_bypass_reason 'grep foo .')
assert_eq "env var triggers bypass" "$reason" "env:LEGION_BYPASS_GREP=1"
unset LEGION_BYPASS_GREP

reason=$(legion_prequery_bypass_reason 'grep foo . # legion-bypass: searching literal string')
assert_eq "comment sentinel triggers bypass" "$reason" "searching literal string"

reason=$(legion_prequery_bypass_reason 'grep foo .')
assert_empty "no bypass reason on plain command" "$reason"

# ---------- pre-bash-grep.sh end-to-end tests ----------

echo "==> hook end-to-end: non-search command passes through"
out=$(echo '{"cwd":"/tmp/legion","tool_name":"Bash","tool_input":{"command":"ls -la"},"session_id":"t"}' | bash "$HOOK")
assert_empty "ls passes through silently" "$out"

echo "==> hook end-to-end: non-symbol pattern passes through"
out=$(echo '{"cwd":"/tmp/legion","tool_name":"Bash","tool_input":{"command":"grep -r foo|bar src/"},"session_id":"t"}' | bash "$HOOK")
assert_empty "regex pattern -> pass through" "$out"

echo "==> hook end-to-end: indexed repo + symbol-shape pattern with LOCAL-REPO hit -> BLOCK"
out=$(echo '{"cwd":"/tmp/legion","tool_name":"Bash","tool_input":{"command":"grep -r Symbol src/"},"session_id":"block-t"}' | bash "$HOOK")
assert_contains "deny decision present" "$out" '"permissionDecision": "deny"'
assert_contains "deny reason mentions sym def" "$out" 'legion sym def'
assert_contains "deny reason names the pattern" "$out" 'Symbol'
assert_contains "deny reason offers env bypass" "$out" 'LEGION_BYPASS_GREP=1'
assert_contains "deny reason offers sentinel bypass" "$out" '# legion-bypass:'

echo "==> #713: BLOCK message routes to sym etc for non-symbol shapes, not just the Grep tool"
assert_contains "names find-content for content search" "$out" 'sym etc find-content'
assert_contains "names sym tree for structure" "$out" 'sym tree'
assert_contains "names extract for config/frontmatter fields" "$out" 'sym etc extract'
assert_contains "names find-file for locate-by-name" "$out" 'sym etc find-file'
assert_contains "states sym is not rust-only" "$out" 'not just Rust'

echo "==> #458 relevance gate: cluster-wide hit but NOT in this repo -> pass through (no block)"
# Stub legion returns commonword hits in huttspawn, but the grep target is in /tmp/legion.
# Pre-#458 behavior: would block on those cross-repo hits.
# Post-#458 behavior: relevance gate filters to local hits (empty) and falls through.
out=$(echo '{"cwd":"/tmp/legion","tool_name":"Bash","tool_input":{"command":"grep -r commonword src/"},"session_id":"relevance-t"}' | bash "$HOOK")
assert_not_contains "cross-repo-only hits do not trigger block" "$out" '"permissionDecision": "deny"'

echo "==> harder bypass (#495 follow-up): soft env bypass REFUSED for symbol with local hit"
export LEGION_TEST_MARKER="$WORK/state/bypass-marker.log"
rm -f "$LEGION_TEST_MARKER"
# Symbol resolves to a local-repo SCIP hit, so LEGION_BYPASS_GREP=1
# is refused. The hook emits a deny decision pointing at sym and
# names the hard escape; no telemetry row is written for the refusal.
out=$(echo '{"cwd":"/tmp/legion","tool_name":"Bash","tool_input":{"command":"grep -r Symbol src/"},"session_id":"bypass-env-t"}' | LEGION_BYPASS_GREP=1 bash "$HOOK")
assert_contains "soft env bypass refused on local symbol" "$out" '"permissionDecision": "deny"'
assert_contains "refusal points to sym list, not a hard escape" "$out" 'sym list'
assert_file_not_contains "refused soft bypass writes no telemetry row" "$LEGION_TEST_MARKER" "record-bypass"
echo "==> #713: bypass-refusal message also routes to sym etc for non-symbol shapes"
assert_contains "refusal names find-content" "$out" 'sym etc find-content'
assert_contains "refusal names sym tree" "$out" 'sym tree'
assert_contains "refusal names extract" "$out" 'sym etc extract'
assert_contains "refusal names find-file" "$out" 'sym etc find-file'

echo "==> harder bypass: soft sentinel bypass REFUSED for symbol with local hit"
rm -f "$LEGION_TEST_MARKER"
out=$(echo '{"cwd":"/tmp/legion","tool_name":"Bash","tool_input":{"command":"grep -r Symbol src/ # legion-bypass: testing"},"session_id":"bypass-sentinel-t"}' | bash "$HOOK")
assert_contains "soft sentinel bypass refused on local symbol" "$out" '"permissionDecision": "deny"'
assert_contains "refusal explains the sentinel is for free text" "$out" 'free-text searches'

echo "==> harder bypass: soft bypass STILL allowed for non-symbol pattern"
# commonword stub returns hits ONLY in unrelated repos; the local
# relevance filter empties LOCAL_HITS, so the soft bypass goes
# through. This is the cross-cutting legitimate case (free-text
# search that happens to look symbol-shaped to the static regex).
rm -f "$LEGION_TEST_MARKER"
out=$(echo '{"cwd":"/tmp/legion","tool_name":"Bash","tool_input":{"command":"grep -r commonword src/ # legion-bypass: free-text counter"},"session_id":"bypass-allow-t"}' | bash "$HOOK")
assert_empty "soft bypass on non-local symbol exits 0" "$out"
assert_file_contains "soft bypass allowed for free-text / non-local pattern" "$LEGION_TEST_MARKER" "record-bypass"

echo "==> #560: LEGION_BYPASS_GREP_HARD is RETIRED -- it no longer escapes the block"
# The frictionless hard escape is gone; mandatory shell-grep blocking is the
# operator's permissions.deny. Setting the old env var must NOT bypass: a
# symbol with a local hit still blocks.
out=$(echo '{"cwd":"/tmp/legion","tool_name":"Bash","tool_input":{"command":"grep -r Symbol src/"},"session_id":"hard-gone-t"}' | LEGION_BYPASS_GREP_HARD=1 bash "$HOOK")
assert_contains "retired hard-bypass env no longer escapes -- still blocks" "$out" '"permissionDecision": "deny"'

echo "==> hook end-to-end: skip via LEGION_SKIP_PRE_BASH_GREP=1"
out=$(echo '{"cwd":"/tmp/legion","tool_name":"Bash","tool_input":{"command":"grep -r Symbol src/"},"session_id":"skip-t"}' | LEGION_SKIP_PRE_BASH_GREP=1 bash "$HOOK")
assert_empty "skip env exits 0" "$out"

echo "==> hook end-to-end: non-Bash tool passes through"
out=$(echo '{"cwd":"/tmp/legion","tool_name":"Read","tool_input":{"command":"grep -r Symbol src/"},"session_id":"t"}' | bash "$HOOK")
assert_empty "non-Bash tool ignored" "$out"

echo "==> LEGION_REPO precedence: env overrides basename(cwd)"
# cwd basename says "legion" (covered + indexed), but LEGION_REPO points at
# an uncovered repo -- the hook must follow LEGION_REPO and pass through.
out=$(echo '{"cwd":"/tmp/legion","tool_name":"Bash","tool_input":{"command":"grep -r Symbol src/"},"session_id":"repo-env-t"}' | LEGION_REPO=uncovered-elsewhere bash "$HOOK")
assert_empty "LEGION_REPO redirects the coverage gate" "$out"


# --- #829: searches spelled as git subcommands ------------------------------
#
# Both enforcement points matched on the first token only, so `git grep`
# and friends slipped past unblocked AND uncounted. These assert the
# detector now names each shape, and that ordinary `git log` still passes.

echo "==> detects git-spelled searches"
assert_eq "git grep detected" \
  "$(legion_prequery_bash_binary 'git grep -n foo -- src')" "git grep"
assert_eq "git ls-files detected" \
  "$(legion_prequery_bash_binary 'git ls-files "*.rs"')" "git ls-files"
assert_eq "git log -S detected" \
  "$(legion_prequery_bash_binary 'git log -S find_plugin --oneline')" "git log -S"
assert_eq "git log -G detected" \
  "$(legion_prequery_bash_binary 'git log -G regex_thing')" "git log -G"
assert_eq "git log --grep detected" \
  "$(legion_prequery_bash_binary 'git log --grep worksource')" "git log --grep"

echo "==> resolves git by basename and past global options"
assert_eq "absolute path git" \
  "$(legion_prequery_bash_binary '/opt/homebrew/bin/git grep foo')" "git grep"
assert_eq "global -C before subcommand" \
  "$(legion_prequery_bash_binary 'git -C /tmp/x grep foo')" "git grep"
assert_eq "global --no-pager" \
  "$(legion_prequery_bash_binary 'git --no-pager grep foo')" "git grep"

echo "==> ordinary git log is not a search"
assert_eq "git log --oneline passes" \
  "$(legion_prequery_bash_binary 'git log --oneline')" ""
assert_eq "git log -p passes" \
  "$(legion_prequery_bash_binary 'git log -p HEAD~3')" ""
assert_eq "bare git log passes" \
  "$(legion_prequery_bash_binary 'git log')" ""
assert_eq "git status passes" \
  "$(legion_prequery_bash_binary 'git status')" ""
assert_eq "git commit passes" \
  "$(legion_prequery_bash_binary 'git commit -m grep')" ""

echo "==> extracts the pattern from each git shape"
assert_eq "git grep positional" \
  "$(legion_prequery_git_pattern 'git grep -n find_plugin -- src' 'git grep')" "find_plugin"
assert_eq "git ls-files glob" \
  "$(legion_prequery_git_pattern 'git ls-files *.rs' 'git ls-files')" "*.rs"
assert_eq "git log -S detached value" \
  "$(legion_prequery_git_pattern 'git log -S find_plugin' 'git log -S')" "find_plugin"
assert_eq "git log -S attached value" \
  "$(legion_prequery_git_pattern 'git log -Sfind_plugin' 'git log -S')" "find_plugin"
assert_eq "git log --grep equals form" \
  "$(legion_prequery_git_pattern 'git log --grep=worksource' 'git log --grep')" "worksource"
assert_eq "git log --grep detached form" \
  "$(legion_prequery_git_pattern 'git log --grep worksource' 'git log --grep')" "worksource"

echo "==> git shapes are classified as such"
legion_prequery_is_git_shape "git grep" && echo "  PASS: git grep is a git shape" || echo "  FAIL: git grep is a git shape"
legion_prequery_is_git_shape "grep" && echo "  FAIL: plain grep misclassified" || echo "  PASS: plain grep is not a git shape"

finish_tests
