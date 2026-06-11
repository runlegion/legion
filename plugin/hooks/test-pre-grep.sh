#!/bin/bash
# Test runner for pre-grep.sh, the merged Grep/Glob search guard (#614).
#
# Covers the union of the former test-pre-grep-recall.sh and
# test-pre-grep-scip.sh surfaces plus the ladder behavior the merge adds
# (block on local sym hit, #458 relevance gate, bypass refusal) so
# Grep-tool behavior provably matches pre-bash-grep on the same pattern.
#
# Run from anywhere:
#   bash plugin/hooks/test-pre-grep.sh

set -u

# shellcheck source=tests/testutil.sh
source "$(dirname "${BASH_SOURCE[0]}")/tests/testutil.sh"

make_plugin_root pre-grep.sh

# Fixtures: repo "legion" is covered and indexed; Symbol resolves to a
# local-repo sym hit, commonword to a cross-repo hit that the relevance
# gate must keep out of the block tier.
export FAKE_WATCH="legion	/tmp/legion"
export FAKE_STATS="legion:5"
export FAKE_INDEX_JSON='[{"repo":"legion","lang":"rust","size_bytes":100,"updated_at":"2026-01-01T00:00:00Z"}]'
export FAKE_SYM_LOCAL="Symbol find_pending_signals"
export FAKE_SYM_LOCAL_REPO="legion"
export FAKE_SYM_REMOTE="commonword"
export FAKE_SYM_REMOTE_REPO="huttspawn"

HOOK="$CLAUDE_PLUGIN_ROOT/hooks/pre-grep.sh"

run_hook() {
  bash "$HOOK" 2>/dev/null
}

echo "==> skip env overrides"
out=$(echo '{"cwd":"/tmp/legion","tool_name":"Grep","tool_input":{"pattern":"Symbol"},"session_id":"skip-t"}' | LEGION_SKIP_PRE_GREP=1 run_hook)
assert_empty "LEGION_SKIP_PRE_GREP=1 produces no stdout" "$out"
out=$(echo '{"cwd":"/tmp/legion","tool_name":"Grep","tool_input":{"pattern":"Symbol"},"session_id":"skip-t2"}' | LEGION_SKIP_PRE_GREP_SCIP=1 run_hook)
assert_empty "legacy LEGION_SKIP_PRE_GREP_SCIP=1 alias still skips" "$out"

echo "==> input validation"
out=$(echo '{"tool_name":"Grep","tool_input":{"pattern":"authentication tokens"}}' | run_hook)
assert_empty "missing cwd exits silently" "$out"
out=$(echo '{"cwd":"/tmp/legion","tool_input":{"pattern":"authentication tokens"}}' | run_hook)
assert_empty "missing tool_name exits silently" "$out"
out=$(printf '' | run_hook)
assert_rc "empty stdin exits 0" 0 $?
assert_empty "empty stdin produces no stdout" "$out"
out=$(echo '{this is not valid json' | run_hook)
assert_rc "malformed JSON exits 0" 0 $?
assert_empty "malformed JSON produces no stdout" "$out"

echo "==> non-Grep/Glob tools exit silently"
out=$(echo '{"cwd":"/tmp/legion","tool_name":"Edit","tool_input":{"file_path":"/tmp/x"},"session_id":"t"}' | run_hook)
assert_empty "Edit tool is not handled" "$out"
out=$(echo '{"cwd":"/tmp/legion","tool_name":"Bash","tool_input":{"command":"ls"},"session_id":"t"}' | run_hook)
assert_empty "Bash tool is not handled" "$out"

echo "==> uncovered repo skips"
out=$(echo '{"cwd":"/tmp/uncovered","tool_name":"Grep","tool_input":{"pattern":"find_pending_signals"},"session_id":"unc-t"}' | run_hook)
assert_empty "uncovered repo with valid symbol -> skip" "$out"

echo "==> sym tier: indexed repo + local hit -> BLOCK (parity with pre-bash-grep)"
out=$(echo '{"cwd":"/tmp/legion","tool_name":"Grep","tool_input":{"pattern":"Symbol"},"session_id":"block-t"}' | run_hook)
assert_contains "deny decision present" "$out" '"permissionDecision": "deny"'
assert_contains "deny reason mentions sym def" "$out" 'legion sym def'
assert_contains "deny reason names the pattern" "$out" 'Symbol'

echo "==> sym tier: word-boundary-wrapped pattern still resolves"
out=$(printf '%s\n' '{"cwd":"/tmp/legion","tool_name":"Grep","tool_input":{"pattern":"\\bSymbol\\b"},"session_id":"wb-t"}' | run_hook)
assert_contains "stripped \\b pattern blocks too" "$out" '"permissionDecision": "deny"'

echo "==> #458 relevance gate: cluster-wide hit only -> INJECT, not block"
out=$(echo '{"cwd":"/tmp/legion","tool_name":"Grep","tool_input":{"pattern":"commonword"},"session_id":"rel-t"}' | run_hook)
assert_not_contains "cross-repo-only hits do not block" "$out" '"permissionDecision": "deny"'
assert_contains "cross-repo hits still injected as context" "$out" '"permissionDecision": "allow"'
assert_contains "injected context carries the sym hits" "$out" 'huttspawn'

echo "==> sym tier: not-indexed repo with local-shaped hit -> INJECT, not block"
out=$(echo '{"cwd":"/tmp/legion","tool_name":"Grep","tool_input":{"pattern":"Symbol"},"session_id":"noidx-t"}' | FAKE_INDEX_JSON='[]' run_hook)
assert_not_contains "no index -> block tier disabled" "$out" '"permissionDecision": "deny"'
assert_contains "no index -> sym hits injected instead" "$out" '"permissionDecision": "allow"'

echo "==> bypass: refused for a symbol with a local hit"
out=$(echo '{"cwd":"/tmp/legion","tool_name":"Grep","tool_input":{"pattern":"Symbol"},"session_id":"byp-t"}' | LEGION_BYPASS_GREP=1 run_hook)
assert_contains "bypass refused on local symbol" "$out" '"permissionDecision": "deny"'
assert_contains "refusal explains the escape is for free text" "$out" 'free-text searches'

echo "==> bypass: allowed + telemetried for non-local pattern"
export LEGION_TEST_MARKER="$WORK/state/bypass-marker.log"
rm -f "$LEGION_TEST_MARKER"
out=$(echo '{"cwd":"/tmp/legion","tool_name":"Grep","tool_input":{"pattern":"commonword"},"session_id":"byp-allow-t"}' | LEGION_BYPASS_GREP=1 run_hook)
assert_empty "bypass on non-local symbol exits 0 silently" "$out"
assert_file_contains "bypass writes telemetry row" "$LEGION_TEST_MARKER" "record-bypass"
assert_file_contains "telemetry row names the Grep tool" "$LEGION_TEST_MARKER" "Grep"

echo "==> recall tier: regex-heavy patterns skip injection"
out=$(printf '%s\n' '{"cwd":"/tmp/legion","tool_name":"Grep","tool_input":{"pattern":"\\s+\\w+"},"session_id":"rx-t"}' | run_hook)
assert_empty "regex-heavy pattern with no tokens skips" "$out"
out=$(echo '{"cwd":"/tmp/legion","tool_name":"Grep","tool_input":{"pattern":"[A-Z][a-z]+"},"session_id":"rx-t2"}' | run_hook)
assert_empty "character class only skips" "$out"
out=$(echo '{"cwd":"/tmp/legion","tool_name":"Grep","tool_input":{"pattern":"fn"},"session_id":"rx-t3"}' | run_hook)
assert_empty "single-token pattern skips" "$out"

echo "==> recall tier: multi-token semantic pattern injects recall hits"
out=$(echo '{"cwd":"/tmp/legion","tool_name":"Grep","tool_input":{"pattern":"hook output format"},"session_id":"rc-t"}' \
  | FAKE_RECALL='- hooks emit permissionDecision JSON (id: 019e0000-0000-7000-8000-000000000000, score: 0.82)' run_hook)
assert_contains "recall hits injected" "$out" 'Legion memory for this query'
assert_contains "injection is an allow decision" "$out" '"permissionDecision": "allow"'

echo "==> recall tier: all scores below 0.3 threshold skip injection"
out=$(echo '{"cwd":"/tmp/legion","tool_name":"Grep","tool_input":{"pattern":"hook output format"},"session_id":"rc-low-t"}' \
  | FAKE_RECALL='- weak match (id: 019e0000-0000-7000-8000-000000000001, score: 0.12)' run_hook)
assert_empty "below-threshold hits skip injection" "$out"

echo "==> recall tier: no recall hits -> silent pass-through"
out=$(echo '{"cwd":"/tmp/legion","tool_name":"Grep","tool_input":{"pattern":"hook output format"},"session_id":"rc-none-t"}' | run_hook)
assert_empty "no hits, no JSON" "$out"

echo "==> Glob wildcard handling"
out=$(echo '{"cwd":"/tmp/legion","tool_name":"Glob","tool_input":{"pattern":"**/*.rs"},"session_id":"gl-t"}' | run_hook)
assert_empty "pure wildcard **/*.rs skips" "$out"
out=$(echo '{"cwd":"/tmp/legion","tool_name":"Glob","tool_input":{"pattern":"*.toml"},"session_id":"gl-t2"}' | run_hook)
assert_empty "pure wildcard *.toml skips" "$out"
out=$(echo '{"cwd":"/tmp/legion","tool_name":"Glob","tool_input":{"pattern":"***"},"session_id":"gl-t3"}' | run_hook)
assert_empty "pure *** skips" "$out"

echo "==> Glob with semantic segments reaches recall"
out=$(echo '{"cwd":"/tmp/legion","tool_name":"Glob","tool_input":{"pattern":"src/**/auth.rs"},"session_id":"gl-sem-t"}' \
  | FAKE_RECALL='- auth flow notes (id: 019e0000-0000-7000-8000-000000000002, score: 0.71)' run_hook)
assert_contains "semantic glob injects recall hits" "$out" 'Legion memory for this query'

echo "==> LEGION_REPO precedence: env overrides basename(cwd)"
out=$(echo '{"cwd":"/tmp/legion","tool_name":"Grep","tool_input":{"pattern":"Symbol"},"session_id":"repo-env-t"}' | LEGION_REPO=uncovered-elsewhere run_hook)
assert_empty "LEGION_REPO redirects the coverage gate" "$out"

finish_tests
