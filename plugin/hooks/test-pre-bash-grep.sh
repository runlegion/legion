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


# --- #876: REWRITE tier -- grep-shaped searches -> find-content -------------
#
# grep/rg/`git grep` are content-search shapes `legion sym etc find-content`
# can answer exactly. Convert deny-and-explain (or, below the symbol-hit
# threshold, silent pass-through) into a rewrite when it is lossless.
#
# #886 review revised this twice:
#   Decision 1 -- the rewrite tier must not beat the BLOCK tier for a
#     pattern that resolves to a LOCAL symbol (`sym def` is a strictly
#     better, non-path-scoped answer). Most tests below therefore use
#     "search_target", a pattern that is NOT in FAKE_SYM_LOCAL/REMOTE, so
#     they exercise the rewrite tier itself rather than the guard in front
#     of it. Fixture patterns ("Symbol", "fn_main", "commonword") are used
#     ONLY in the dedicated Decision 1 tests further down.
#   Decision 2 -- plain `grep` (no gitignore/hidden awareness, unlike `rg`
#     and unlike find-content's own default) now DENIES instead of
#     rewriting with a disclosure. Tests that exist to prove
#     `_legion_bashgrep_classify`'s FLAG/PATTERN translation logic (not
#     grep's scope decision) now use `rg` as the binary, since rg exercises
#     the identical classifier and still successfully rewrites.

echo "==> #876 REWRITE: bare git grep (no path scoping) rewrites, adds --hidden for dotfile parity"
out=$(echo '{"cwd":"/tmp/legion","tool_name":"Bash","tool_input":{"command":"git grep search_target"},"session_id":"rw-gitgrep-t"}' | bash "$HOOK")
assert_contains "updatedInput present" "$out" '"updatedInput"'
assert_contains "rewritten command names find-content" "$out" 'sym etc find-content'
assert_contains "rewritten command carries the pattern" "$out" 'search_target'
assert_contains "rewritten command scopes to repo" "$out" '--repo legion'
assert_contains "git grep rewrite adds --hidden for tracked-dotfile parity" "$out" '--hidden'
assert_contains "CTX announces the translation" "$out" 'Translated your'
assert_not_contains "git grep rewrite never adds --no-ignore (secrets hazard)" "$out" 'no-ignore'

echo "==> #876 REWRITE: git grep -- . (trivial path) still rewrites"
out=$(echo '{"cwd":"/tmp/legion","tool_name":"Bash","tool_input":{"command":"git grep -n search_target -- ."},"session_id":"rw-gitgrep-dot-t"}' | bash "$HOOK")
assert_contains "trivial . path still rewrites" "$out" '"updatedInput"'

echo "==> #876 REWRITE: git grep with a real subdirectory pathspec falls through unchanged, even with no local symbol to redirect to (classify's own path-arg check, independent of the Decision 1 guard)"
out=$(echo '{"cwd":"/tmp/legion","tool_name":"Bash","tool_input":{"command":"git grep -n search_target -- src"},"session_id":"rw-gitgrep-path-t"}' | bash "$HOOK")
assert_not_contains "path-scoped git grep is not rewritten" "$out" '"updatedInput"'
assert_empty "no local symbol either, so this is a silent pass -- not a BLOCK" "$out"

echo "==> #876 REWRITE: rg PAT rewrites with no scope caveat (rg's own defaults already match find-content's)"
out=$(echo '{"cwd":"/tmp/legion","tool_name":"Bash","tool_input":{"command":"rg search_target"},"session_id":"rw-rg-t"}' | bash "$HOOK")
assert_contains "rg rewrite present" "$out" '"updatedInput"'
assert_not_contains "rg rewrite has no gitignore caveat" "$out" 'gitignored files or dotfiles'

echo "==> #876 REWRITE: -F/--fixed-strings translates directly (via rg -- plain grep would now DENY on scope, see Decision 2 tests below)"
out=$(echo '{"cwd":"/tmp/legion","tool_name":"Bash","tool_input":{"command":"rg -F search_target ."},"session_id":"rw-fixed-t"}' | bash "$HOOK")
assert_contains "fixed-strings carried through" "$out" '--fixed-strings'

echo "==> #876 REWRITE: -i translates to an inline (?i) regex prefix"
out=$(echo '{"cwd":"/tmp/legion","tool_name":"Bash","tool_input":{"command":"rg -i search_target ."},"session_id":"rw-icase-t"}' | bash "$HOOK")
# printf %q shell-escapes the parens/`?` in `(?i)` for safe re-execution
# (they are shell metacharacters), so the raw JSON carries the escaped
# form -- assert on the substring that survives escaping either way.
assert_contains "case-insensitive prefix applied" "$out" '?i'

echo "==> #876 REWRITE: --include=*.rs (single simple extension) translates to --ext rs"
out=$(echo '{"cwd":"/tmp/legion","tool_name":"Bash","tool_input":{"command":"rg -rn search_target --include=*.rs ."},"session_id":"rw-include-t"}' | bash "$HOOK")
assert_contains "--include=*.rs becomes --ext rs" "$out" '--ext rs'

echo "==> #876 REWRITE: leading-dash pattern via -e survives quoting intact (known hazard: a hyphen-leading pattern must not silently misparse)"
out=$(echo '{"cwd":"/tmp/legion","tool_name":"Bash","tool_input":{"command":"rg -rn -e --domain-thing ."},"session_id":"rw-dash-pattern-t"}' | bash "$HOOK")
assert_contains "leading-dash pattern (via -e) rewrites" "$out" '"updatedInput"'
assert_contains "pattern is carried into the rewritten command" "$out" 'domain-thing'

echo "==> #876 DECISION 1 (#886 review): a pattern that resolves to a LOCAL symbol falls through to the BLOCK tier instead of rewriting, even for an otherwise-clean bare git grep"
out=$(echo '{"cwd":"/tmp/legion","tool_name":"Bash","tool_input":{"command":"git grep fn_main"},"session_id":"decision1-local-t"}' | bash "$HOOK")
assert_not_contains "local symbol -- NOT rewritten" "$out" '"updatedInput"'
assert_contains "falls through to the BLOCK tier's sym def guidance instead" "$out" 'legion sym def'
assert_contains "BLOCK reason names the pattern" "$out" 'fn_main'

echo "==> #876 DECISION 1: a cross-repo-ONLY hit (filtered out of LOCAL_HITS by the #458 relevance gate) still rewrites -- the guard checks LOCAL_HITS, not HAD_SYM"
out=$(echo '{"cwd":"/tmp/legion","tool_name":"Bash","tool_input":{"command":"git grep commonword"},"session_id":"decision1-remote-t"}' | bash "$HOOK")
assert_contains "remote-only hit does not block the rewrite" "$out" '"updatedInput"'

echo "==> #876 DECISION 2 (#886 review): plain grep with no gitignore/hidden awareness now DENIES (was: rewrite + prose disclosure)"
out=$(echo '{"cwd":"/tmp/legion","tool_name":"Bash","tool_input":{"command":"grep -rn search_target ."},"session_id":"decision2-bare-t"}' | bash "$HOOK")
assert_contains "plain grep denied, not rewritten" "$out" '"permissionDecision": "deny"'
assert_not_contains "no updatedInput -- a narrowed-scope rewrite is not offered silently" "$out" '"updatedInput"'
assert_contains "denial names the gitignore/hidden gap" "$out" 'gitignore'
assert_contains "denial points at find-content with --hidden as the closest equivalent" "$out" '--hidden'
# The message DOES mention --no-ignore -- to explain why it will not be
# added, which requires naming it. What must never appear is the two
# flags suggested TOGETHER as a runnable command.
assert_not_contains "the suggested command never bundles --hidden with --no-ignore (secrets hazard)" "$out" '\-\-hidden \-\-no-ignore'

echo "==> #876 DECISION 2: the grep-scope deny fires even when the flags WOULD have translated cleanly -- proves it is not merely an unclassified fallback"
out=$(echo '{"cwd":"/tmp/legion","tool_name":"Bash","tool_input":{"command":"grep -F search_target ."},"session_id":"decision2-flag-t"}' | bash "$HOOK")
assert_contains "still denied even though -F alone would have translated fine" "$out" '"permissionDecision": "deny"'

echo "==> #876 consistency check: a shape where the sanctioned extractor and this classifier would disagree does not guess"
# The sanctioned extractor (legion_prequery_extract_pattern) has no special
# handling for a bare `--` end-of-options marker -- it treats `--` and
# whatever leading-dash token follows it as more flags to skip, so for
# `grep -rn -- --spacing-0.5 .` it would derive "." as the pattern. This
# classifier's own `--` handling derives "--spacing-0.5". The mismatch must
# short-circuit the rewrite rather than trust either guess.
out=$(echo '{"cwd":"/tmp/legion","tool_name":"Bash","tool_input":{"command":"grep -rn -- --spacing-0.5 ."},"session_id":"rw-mismatch-t"}' | bash "$HOOK")
assert_not_contains "extractor disagreement -- no rewrite" "$out" '"updatedInput"'
assert_not_contains "extractor disagreement -- no deny either (unclassified, not lossy)" "$out" '"permissionDecision": "deny"'

echo "==> #876 REGRESSION (reported by team-lead): a quoted alternation must never rewrite to a SILENTLY TRUNCATED pattern"
# Root cause: the sanctioned extractor (legion_prequery_extract_pattern in
# _legion-prequery.sh, out of this card's scope) truncates its head on the
# first literal `|`/`;`/`>`/`<` REGARDLESS of quoting -- for
# `grep -E 'foo|bar' .` it derives "foo", not "foo|bar". This classifier's
# OWN extraction is quote-aware (_legion_bashgrep_safe_head) and correctly
# derives the full "foo|bar", which now DISAGREES with the sanctioned
# extractor's "foo" -- the consistency check (RW_PATTERN != $PATTERN) must
# catch that disagreement and refuse to guess, rather than rewrite with
# either value. The correct, safe outcome here is NO rewrite at all (falls
# through to the pre-#876 ladder), not a rewrite carrying a truncated
# pattern that reads as a plausible, wrong answer.
out=$(echo '{"cwd":"/tmp/legion","tool_name":"Bash","tool_input":{"command":"grep -E '"'"'foo|bar'"'"' ."},"session_id":"rw-quoted-pipe-t"}' | bash "$HOOK")
assert_not_contains "quoted alternation is NOT rewritten (sanctioned extractor still truncates it, so we cannot trust either value)" "$out" '"updatedInput"'
assert_not_contains "the truncated pattern never appears as if it were the full answer" "$out" 'find-content foo --repo'

echo "==> #876 REGRESSION: the same truncate-inside-quotes hazard for ; and > and < (all also truncation chars in the sanctioned extractor -- same safe fall-through)"
for pat in 'foo;bar' 'foo>bar' 'foo<bar'; do
  out=$(echo "{\"cwd\":\"/tmp/legion\",\"tool_name\":\"Bash\",\"tool_input\":{\"command\":\"grep -E '$pat' .\"},\"session_id\":\"rw-trunc-$pat-t\"}" | bash "$HOOK")
  assert_not_contains "quoted '$pat' is not rewritten with a truncated value" "$out" '"updatedInput"'
done

echo "==> #876: a quoted single & is NOT a sanctioned-extractor truncation char, so both extractors agree and the FULL pattern (with &) survives a real rewrite"
# rg, not grep -- under Decision 2 plain grep now denies on scope
# regardless of whether the pattern itself would translate cleanly (see the
# dedicated Decision 2 tests above); this test's purpose is pattern-content
# preservation through the classifier, so it uses the binary that still
# reaches a successful rewrite. Needles avoid the backslash printf
# %q/jq double-escape into the raw JSON (a fragile exact-match) --
# checking the text on both sides of & is sufficient proof the pattern was
# not cut at the &.
out=$(echo '{"cwd":"/tmp/legion","tool_name":"Bash","tool_input":{"command":"rg '"'"'fn_main&other'"'"' ."},"session_id":"rw-amp-t"}' | bash "$HOOK")
assert_contains "rewrite happens" "$out" '"updatedInput"'
assert_contains "text before & survives" "$out" 'fn_main'
assert_contains "text after & survives (not truncated there)" "$out" 'other'

echo "==> #876: a quoted pattern using OTHER regex metacharacters (not truncation chars) survives a real rewrite fully intact -- proves the fix is not just suppressing everything"
out=$(echo '{"cwd":"/tmp/legion","tool_name":"Bash","tool_input":{"command":"rg '"'"'fn_main[0-9]+'"'"' ."},"session_id":"rw-brackets-t"}' | bash "$HOOK")
assert_contains "rewrite happens" "$out" '"updatedInput"'
assert_contains "text before the bracket class survives" "$out" 'fn_main'
assert_contains "the character class content survives (not truncated)" "$out" '0-9'

echo "==> #876 compound guard: a real (unquoted) pipe is never rewritten"
out=$(echo '{"cwd":"/tmp/legion","tool_name":"Bash","tool_input":{"command":"grep fn_main . | wc -l"},"session_id":"rw-real-pipe-t"}' | bash "$HOOK")
assert_not_contains "real pipeline is not rewritten" "$out" '"updatedInput"'

# DENY tests below use "search_target" -- not a FAKE_SYM_LOCAL/REMOTE
# fixture, so the Decision 1 guard (a local symbol falls through to BLOCK
# before classify() ever runs) does not mask the flag-lossy DENY these
# tests exist to exercise.

echo "==> #876 DENY: context flags (-A/-B/-C) have no find-content equivalent"
out=$(echo '{"cwd":"/tmp/legion","tool_name":"Bash","tool_input":{"command":"grep -A3 search_target ."},"session_id":"deny-context-t"}' | bash "$HOOK")
assert_contains "context flag denied" "$out" '"permissionDecision": "deny"'
assert_contains "denial names the flag" "$out" 'context lines'

echo "==> #876 DENY: -l (files-with-matches)"
out=$(echo '{"cwd":"/tmp/legion","tool_name":"Bash","tool_input":{"command":"grep -l search_target ."},"session_id":"deny-l-t"}' | bash "$HOOK")
assert_contains "-l denied" "$out" '"permissionDecision": "deny"'
assert_contains "denial explains files-only vs per-line" "$out" 'files-only list'

echo "==> #876 DENY (known hazard): combined -cE cluster + a leading-dash pattern denies on -c, never silently returns an empty green result"
out=$(echo '{"cwd":"/tmp/legion","tool_name":"Bash","tool_input":{"command":"grep -cE \"--domain checkpoint\""},"session_id":"deny-hazard-t"}' | bash "$HOOK")
assert_contains "combined -cE cluster denies on -c" "$out" '"permissionDecision": "deny"'
assert_contains "denial names count mode" "$out" 'count mode'

echo "==> #876 DENY: -o (only-matching)"
out=$(echo '{"cwd":"/tmp/legion","tool_name":"Bash","tool_input":{"command":"grep -o search_target ."},"session_id":"deny-o-t"}' | bash "$HOOK")
assert_contains "-o denied" "$out" '"permissionDecision": "deny"'

echo "==> #876 DENY: -v (invert-match)"
out=$(echo '{"cwd":"/tmp/legion","tool_name":"Bash","tool_input":{"command":"grep -v search_target ."},"session_id":"deny-v-t"}' | bash "$HOOK")
assert_contains "-v denied" "$out" '"permissionDecision": "deny"'

echo "==> #876 DENY: -w (word-regexp)"
out=$(echo '{"cwd":"/tmp/legion","tool_name":"Bash","tool_input":{"command":"grep -w search_target ."},"session_id":"deny-w-t"}' | bash "$HOOK")
assert_contains "-w denied" "$out" '"permissionDecision": "deny"'

echo "==> #876 DENY: -x (line-regexp)"
out=$(echo '{"cwd":"/tmp/legion","tool_name":"Bash","tool_input":{"command":"grep -x search_target ."},"session_id":"deny-x-t"}' | bash "$HOOK")
assert_contains "-x denied" "$out" '"permissionDecision": "deny"'

echo "==> #876 DENY: -P (perl-regexp)"
out=$(echo '{"cwd":"/tmp/legion","tool_name":"Bash","tool_input":{"command":"grep -P search_target ."},"session_id":"deny-p-t"}' | bash "$HOOK")
assert_contains "-P denied" "$out" '"permissionDecision": "deny"'
assert_contains "denial explains PCRE gap" "$out" 'PCRE'

echo "==> #876 DENY: --include with a non-simple-extension glob"
out=$(echo '{"cwd":"/tmp/legion","tool_name":"Bash","tool_input":{"command":"grep -rn search_target --include=*.spec.ts ."},"session_id":"deny-include-t"}' | bash "$HOOK")
assert_contains "complex --include denied" "$out" '"permissionDecision": "deny"'

echo "==> #876 DENY: --exclude has no find-content equivalent"
out=$(echo '{"cwd":"/tmp/legion","tool_name":"Bash","tool_input":{"command":"grep -rn search_target --exclude=vendor.rs ."},"session_id":"deny-exclude-t"}' | bash "$HOOK")
assert_contains "--exclude denied" "$out" '"permissionDecision": "deny"'

echo "==> #876 DENY: -i combined with -F -- no case-insensitive fixed-string equivalent"
out=$(echo '{"cwd":"/tmp/legion","tool_name":"Bash","tool_input":{"command":"grep -iF search_target ."},"session_id":"deny-if-t"}' | bash "$HOOK")
assert_contains "-i + -F combo denied" "$out" '"permissionDecision": "deny"'

echo "==> #876: bypass tier still wins over the rewrite tier"
rm -f "$LEGION_TEST_MARKER"
# "freetextphrase" is not in any FAKE_SYM_* fixture, so this is the
# non-symbol soft-bypass-allowed path, not the refused-on-local-hit path --
# it must never even reach the rewrite classifier.
out=$(echo '{"cwd":"/tmp/legion","tool_name":"Bash","tool_input":{"command":"grep freetextphrase . # legion-bypass: manual check"},"session_id":"rw-bypass-wins-t"}' | bash "$HOOK")
assert_not_contains "bypass wins -- no rewrite" "$out" '"updatedInput"'
assert_empty "bypass allowed silently, same as before #876" "$out"
assert_file_contains "bypass still recorded" "$LEGION_TEST_MARKER" "record-bypass"

echo "==> #876: git log -S / --grep still pass through unchanged, never rewritten"
out=$(echo '{"cwd":"/tmp/legion","tool_name":"Bash","tool_input":{"command":"git log -S find_plugin"},"session_id":"rw-gitlog-s-t"}' | bash "$HOOK")
assert_empty "git log -S is not a content-search rewrite candidate" "$out"

echo "==> #876: ag/ack are left on the existing ladder, not rewritten"
out=$(echo '{"cwd":"/tmp/legion","tool_name":"Bash","tool_input":{"command":"ag fn_main ."},"session_id":"rw-ag-t"}' | bash "$HOOK")
assert_not_contains "ag is not rewritten" "$out" '"updatedInput"'

echo "==> #876: not-legion-covered repo -- no rewrite, same universal gate as everything else"
out=$(echo '{"cwd":"/tmp/legion","tool_name":"Bash","tool_input":{"command":"git grep Symbol"},"session_id":"rw-uncovered-t"}' | LEGION_REPO=uncovered-elsewhere bash "$HOOK")
assert_empty "uncovered repo -- no rewrite, no deny, nothing" "$out"

finish_tests
