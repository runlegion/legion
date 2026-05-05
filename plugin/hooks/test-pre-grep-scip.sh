#!/bin/bash
# Test runner for pre-grep-scip.sh.
#
# Feeds synthetic hook JSON into the hook and asserts the hook behaves
# correctly on each branch. Does NOT require a working legion binary or
# a populated SCIP index: when $LEGION is missing or `legion sym def`
# returns nothing, the hook must exit 0 without emitting JSON, so all
# "no injection expected" tests pass cleanly even in a cold environment.
#
# Run from the repo root:
#   bash plugin/hooks/test-pre-grep-scip.sh
#
# Exits 0 on success, 1 on any failed assertion.

set -u

HOOK="${CLAUDE_PLUGIN_ROOT:-$(pwd)}/plugin/hooks/pre-grep-scip.sh"
if [ ! -f "$HOOK" ]; then
  echo "FAIL: hook not found at $HOOK" >&2
  exit 1
fi

PASS=0
FAIL=0

# Reads stdin (the synthetic hook event) and asserts the hook produced no
# stdout. Uses a heredoc-style invocation from callers so the function runs
# in the parent shell and PASS/FAIL counters survive.
assert_empty_stdout() {
  local desc="$1"
  local input
  input=$(cat)
  local output
  output=$(echo "$input" | CLAUDE_PLUGIN_ROOT="${CLAUDE_PLUGIN_ROOT:-$(pwd)}/plugin" bash "$HOOK" 2>/dev/null)
  if [ -z "$output" ]; then
    PASS=$((PASS + 1))
    echo "  PASS: $desc"
  else
    FAIL=$((FAIL + 1))
    echo "  FAIL: $desc"
    echo "    expected empty stdout, got: $output" >&2
  fi
}

echo "==> regex-noisy patterns skip injection"

assert_empty_stdout "open paren in pattern -> skip" <<'EOF'
{"cwd":"/tmp","tool_name":"Grep","tool_input":{"pattern":"abc(def"},"session_id":"t"}
EOF

assert_empty_stdout "alternation in pattern -> skip" <<'EOF'
{"cwd":"/tmp","tool_name":"Grep","tool_input":{"pattern":"foo|bar"},"session_id":"t"}
EOF

assert_empty_stdout "anchored pattern -> skip" <<'EOF'
{"cwd":"/tmp","tool_name":"Grep","tool_input":{"pattern":"^start"},"session_id":"t"}
EOF

assert_empty_stdout "whitespace in pattern -> skip" <<'EOF'
{"cwd":"/tmp","tool_name":"Grep","tool_input":{"pattern":"two words"},"session_id":"t"}
EOF

echo "==> short / non-identifier patterns skip"

assert_empty_stdout "length 2 -> skip" <<'EOF'
{"cwd":"/tmp","tool_name":"Grep","tool_input":{"pattern":"to"},"session_id":"t"}
EOF

assert_empty_stdout "starts with digit -> skip" <<'EOF'
{"cwd":"/tmp","tool_name":"Grep","tool_input":{"pattern":"123abc"},"session_id":"t"}
EOF

echo "==> non-Grep/Glob tools skip"

assert_empty_stdout "Read tool -> skip" <<'EOF'
{"cwd":"/tmp","tool_name":"Read","tool_input":{"file_path":"/tmp/foo"},"session_id":"t"}
EOF

assert_empty_stdout "Bash tool -> skip" <<'EOF'
{"cwd":"/tmp","tool_name":"Bash","tool_input":{"command":"ls"},"session_id":"t"}
EOF

echo "==> missing fields skip"

assert_empty_stdout "empty event -> skip" <<'EOF'
{}
EOF

assert_empty_stdout "missing pattern -> skip" <<'EOF'
{"cwd":"/tmp","tool_name":"Grep"}
EOF

echo "==> uncovered repo skips"

# /tmp is never legion-covered.
assert_empty_stdout "uncovered repo with valid symbol -> skip" <<'EOF'
{"cwd":"/tmp","tool_name":"Grep","tool_input":{"pattern":"find_pending_signals"},"session_id":"t"}
EOF

echo
echo "---"
echo "Results: $PASS passed, $FAIL failed"
if [ "$FAIL" -gt 0 ]; then
  exit 1
fi
exit 0
