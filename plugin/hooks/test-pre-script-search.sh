#!/bin/bash
# Test runner for the pre-script-search PreToolUse hook (#837).
#
# The observed case: an agent writes a Python script to walk a directory
# tree in a covered, indexed repo where `legion sym tree` already answers.
# No first-token rule can catch that, so this hook watches the content
# being written and the inline code being run.
#
# Two properties matter more than coverage breadth here:
#   1. It INJECTS, never denies. Denying real work to prevent a redundant
#      listing is the worse trade.
#   2. It does not over-fire. A script that merely mentions a path, or
#      does structured editing, must pass through untouched.
#
#   bash plugin/hooks/test-pre-script-search.sh

set -u

# shellcheck source=tests/testutil.sh
source "$(dirname "${BASH_SOURCE[0]}")/tests/testutil.sh"

make_plugin_root pre-script-search.sh

export FAKE_WATCH="legion-test	/tmp/legion-test"
# The hook refuses to point at sym in an unindexed repo -- that would be
# advice the agent cannot take -- so the covered fixture must also report
# an index.
export FAKE_INDEX_JSON='[{"repo":"legion-test","lang":"rust","size_bytes":100,"updated_at":"2026-01-01T00:00:00Z"}]'

HOOK="$CLAUDE_PLUGIN_ROOT/hooks/pre-script-search.sh"

run_write() {
  jq -n --arg c "$1" '{tool_name:"Write",tool_input:{content:$c},cwd:"/tmp/legion-test",session_id:"test"}' | bash "$HOOK"
}
run_bash() {
  jq -n --arg c "$1" '{tool_name:"Bash",tool_input:{command:$c},cwd:"/tmp/legion-test",session_id:"test"}' | bash "$HOOK"
}

echo "==> the observed rafters case: a script that walks a tree"
out=$(run_write 'import os
for root, dirs, files in os.walk("."):
    print(root)')
assert_contains "suggests sym tree" "$out" 'legion sym tree --repo legion-test'
assert_contains "injects rather than denies" "$out" '"permissionDecision": "allow"'
assert_not_contains "never denies" "$out" '"permissionDecision": "deny"'

echo "==> routes each shape to ONE command, not a menu"
assert_contains "rglob -> tree" "$(run_write 'Path(".").rglob("*.rs")')" 'legion sym tree'
assert_contains "readdirSync -> tree" "$(run_write 'fs.readdirSync(dir)')" 'legion sym tree'
assert_contains "fnmatch -> find-file" "$(run_write 'import fnmatch
fnmatch.filter(names, "*.rs")')" 'find-file'
assert_contains "re.search -> find-content" "$(run_write 'import re
re.search(pat, text)')" 'find-content'
assert_not_contains "tree answer does not also list find-content" \
  "$(run_write 'import os
os.walk(".")')" 'find-content'

echo "==> inline interpreter code on Bash"
assert_contains "python -c walk" "$(run_bash 'python3 -c "import os; os.walk(\".\")"')" 'legion sym tree'
assert_contains "node -e readdir" "$(run_bash 'node -e "fs.readdirSync(\".\")"')" 'legion sym tree'
assert_contains "absolute-path interpreter" "$(run_bash '/usr/bin/python3 -c "import os; os.walk(\".\")"')" 'legion sym tree'

echo "==> does NOT over-fire"
assert_empty "ordinary script passes" "$(run_write 'def add(a, b):
    return a + b')"
assert_empty "structured json edit passes" "$(run_write 'import json
d = json.load(open("f.json"))
json.dump(d, open("f.json", "w"))')"
assert_empty "script named by path, no inline code" "$(run_bash 'python3 tree.py')"
assert_empty "interpreter with no search primitive" "$(run_bash 'python3 -c "print(1 + 1)"')"
assert_empty "non-interpreter bash passes" "$(run_bash 'ls -la')"
assert_empty "cargo build passes" "$(run_bash 'cargo build --release')"

echo "==> gates"
out_unc=$(jq -n '{tool_name:"Write",tool_input:{content:"import os\nos.walk(\".\")"},cwd:"/tmp/not-a-legion-repo",session_id:"test"}' | bash "$HOOK")
assert_empty "uncovered repo passes through" "$out_unc"
assert_empty "skip switch honoured" "$(LEGION_SKIP_SCRIPT_SEARCH=1 run_write 'import os
os.walk(".")')"

finish_tests
