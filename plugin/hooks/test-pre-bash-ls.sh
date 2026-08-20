#!/bin/bash
# Test runner for pre-bash-ls.sh (#976).
#
# Uses the shared fake plugin root + parameterized stub legion from
# tests/testutil.sh. Each test feeds synthetic hook JSON over stdin and
# asserts on stdout shape. Run from anywhere:
#
#   bash plugin/hooks/test-pre-bash-ls.sh
#
# Exits 0 on success, 1 on any failed assertion.

set -u

# shellcheck source=tests/testutil.sh
source "$(dirname "${BASH_SOURCE[0]}")/tests/testutil.sh"

make_plugin_root pre-bash-ls.sh

# Fixtures: repo "legion" is covered (watch) and indexed at /tmp/legion.
# Repo "bare" is covered (watch) but NOT indexed -- exercises the index gate.
export FAKE_WATCH="legion	/tmp/legion
bare	/tmp/bare"
export FAKE_STATS="legion:5"
export FAKE_INDEX_JSON='[{"repo":"legion","lang":"rust","size_bytes":100,"updated_at":"2026-01-01T00:00:00Z"}]'

HOOK="$CLAUDE_PLUGIN_ROOT/hooks/pre-bash-ls.sh"

# ---------- inject path ----------

echo "==> bare ls in an indexed repo -> inject (allow + sym suggestion)"
out=$(echo '{"cwd":"/tmp/legion","tool_name":"Bash","tool_input":{"command":"ls"},"session_id":"ls-bare-t"}' | bash "$HOOK")
assert_contains "allow decision present" "$out" '"permissionDecision": "allow"'
assert_contains "additionalContext present" "$out" 'additionalContext'
assert_contains "suggests sym tree" "$out" 'legion sym tree --repo legion'
assert_contains "suggests sym list" "$out" 'legion sym list --repo legion'
assert_contains "notes ls still ran" "$out" 'still ran'

echo "==> ls -la in an indexed repo -> inject, NOT block (ls is not lossless)"
out=$(echo '{"cwd":"/tmp/legion","tool_name":"Bash","tool_input":{"command":"ls -la"},"session_id":"ls-la-t"}' | bash "$HOOK")
assert_contains "ls -la still injects" "$out" 'legion sym tree --repo legion'
assert_not_contains "ls -la is never a deny" "$out" '"permissionDecision": "deny"'

echo "==> ls of a sub-path inside an indexed repo -> inject"
out=$(echo '{"cwd":"/tmp/legion","tool_name":"Bash","tool_input":{"command":"ls src/"},"session_id":"ls-sub-t"}' | bash "$HOOK")
assert_contains "sub-path listing injects" "$out" 'legion sym tree --repo legion'

echo "==> ls of an absolute path INSIDE the repo tree -> inject"
out=$(echo '{"cwd":"/tmp/legion","tool_name":"Bash","tool_input":{"command":"ls /tmp/legion/src"},"session_id":"ls-abs-in-t"}' | bash "$HOOK")
assert_contains "absolute in-tree listing injects" "$out" 'legion sym tree --repo legion'

echo "==> ls with several targets, ANY inside the repo -> inject"
out=$(echo '{"cwd":"/tmp/legion","tool_name":"Bash","tool_input":{"command":"ls /tmp/scratch src/"},"session_id":"ls-multi-t"}' | bash "$HOOK")
assert_contains "any-in-tree target injects" "$out" 'legion sym tree --repo legion'

# ---------- pass-through path ----------

echo "==> ls of a scratch dir from within an indexed repo -> pass through"
out=$(echo '{"cwd":"/tmp/legion","tool_name":"Bash","tool_input":{"command":"ls /tmp/scratch"},"session_id":"ls-scratch-t"}' | bash "$HOOK")
assert_empty "out-of-tree absolute path passes through" "$out"

echo "==> ls .. (parent outside cwd) from within an indexed repo -> pass through"
out=$(echo '{"cwd":"/tmp/legion","tool_name":"Bash","tool_input":{"command":"ls .."},"session_id":"ls-parent-t"}' | bash "$HOOK")
assert_empty "parent-escaping relative path passes through" "$out"

echo "==> bare ls in a non-indexed (uncovered) cwd -> pass through"
out=$(echo '{"cwd":"/tmp/elsewhere","tool_name":"Bash","tool_input":{"command":"ls"},"session_id":"ls-uncovered-t"}' | bash "$HOOK")
assert_empty "uncovered cwd passes through" "$out"

echo "==> bare ls in a COVERED but NOT-indexed cwd -> pass through (index gate)"
out=$(echo '{"cwd":"/tmp/bare","tool_name":"Bash","tool_input":{"command":"ls"},"session_id":"ls-unindexed-t"}' | bash "$HOOK")
assert_empty "covered-but-unindexed cwd passes through" "$out"

echo "==> a non-ls command passes through"
out=$(echo '{"cwd":"/tmp/legion","tool_name":"Bash","tool_input":{"command":"lsof -i"},"session_id":"lsof-t"}' | bash "$HOOK")
assert_empty "lsof is not ls" "$out"
out=$(echo '{"cwd":"/tmp/legion","tool_name":"Bash","tool_input":{"command":"cat README.md"},"session_id":"cat-t"}' | bash "$HOOK")
assert_empty "cat passes through" "$out"

echo "==> non-Bash tool passes through"
out=$(echo '{"cwd":"/tmp/legion","tool_name":"Read","tool_input":{"command":"ls"},"session_id":"read-t"}' | bash "$HOOK")
assert_empty "non-Bash tool ignored" "$out"

echo "==> skip via LEGION_SKIP_PRE_BASH_LS=1"
out=$(echo '{"cwd":"/tmp/legion","tool_name":"Bash","tool_input":{"command":"ls"},"session_id":"skip-t"}' | LEGION_SKIP_PRE_BASH_LS=1 bash "$HOOK")
assert_empty "skip env exits 0" "$out"

echo "==> LEGION_REPO precedence: env overrides basename(cwd)"
# cwd basename says "legion" (covered + indexed), but LEGION_REPO points at an
# uncovered repo -- the hook must follow LEGION_REPO and pass through.
out=$(echo '{"cwd":"/tmp/legion","tool_name":"Bash","tool_input":{"command":"ls"},"session_id":"repo-env-t"}' | LEGION_REPO=uncovered-elsewhere bash "$HOOK")
assert_empty "LEGION_REPO redirects the coverage gate" "$out"

# ---------- bypass + telemetry ----------

export LEGION_TEST_MARKER="$WORK/state/bypass-marker.log"

echo "==> bypass sentinel -> allow (no injection) + one telemetry row"
rm -f "$LEGION_TEST_MARKER"
out=$(echo '{"cwd":"/tmp/legion","tool_name":"Bash","tool_input":{"command":"ls src/ # legion-bypass: manual structural check"},"session_id":"ls-bypass-t"}' | bash "$HOOK")
assert_empty "bypass suppresses the suggestion" "$out"
assert_file_contains "bypass writes a telemetry row" "$LEGION_TEST_MARKER" "record-bypass"
assert_file_contains "telemetry row carries the ls-structure reason" "$LEGION_TEST_MARKER" "ls-structure"
assert_file_contains "telemetry row carries the resolved target as the pattern" "$LEGION_TEST_MARKER" "/tmp/legion/src"

echo "==> LEGION_BYPASS_GREP=1 env -> allow + telemetry row"
rm -f "$LEGION_TEST_MARKER"
out=$(echo '{"cwd":"/tmp/legion","tool_name":"Bash","tool_input":{"command":"ls"},"session_id":"ls-bypass-env-t"}' | LEGION_BYPASS_GREP=1 bash "$HOOK")
assert_empty "env bypass suppresses the suggestion" "$out"
assert_file_contains "env bypass writes a telemetry row" "$LEGION_TEST_MARKER" "record-bypass"

echo "==> the plain (non-bypass) inject path writes NO telemetry row"
rm -f "$LEGION_TEST_MARKER"
out=$(echo '{"cwd":"/tmp/legion","tool_name":"Bash","tool_input":{"command":"ls"},"session_id":"ls-notele-t"}' | bash "$HOOK")
assert_contains "plain ls still injects" "$out" 'legion sym tree --repo legion'
assert_file_not_contains "plain inject records no bypass row" "$LEGION_TEST_MARKER" "record-bypass"

finish_tests
