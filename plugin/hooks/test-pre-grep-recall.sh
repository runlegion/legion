#!/bin/bash
# Test runner for pre-grep-recall.sh.
#
# Feeds synthetic hook JSON into the hook and asserts the hook behaves
# correctly on each branch. Does NOT require a working legion binary: when
# $LEGION is missing or the recall command fails, the hook must exit 0
# without emitting JSON, so all "no injection expected" tests pass cleanly
# even in a cold environment.
#
# Run from the repo root:
#   bash plugin/hooks/test-pre-grep-recall.sh
#
# Exits 0 on success, 1 on any failed assertion.

set -u

HOOK="${CLAUDE_PLUGIN_ROOT:-$(pwd)}/plugin/hooks/pre-grep-recall.sh"
if [ ! -f "$HOOK" ]; then
  echo "FAIL: hook not found at $HOOK" >&2
  exit 1
fi

PASS=0
FAIL=0

# assert_empty_stdout DESCRIPTION -- read input from stdin, assert stdout is empty
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

# assert_exit_zero DESCRIPTION -- read input from stdin, assert exit 0
assert_exit_zero() {
  local desc="$1"
  local input
  input=$(cat)
  echo "$input" | CLAUDE_PLUGIN_ROOT="${CLAUDE_PLUGIN_ROOT:-$(pwd)}/plugin" bash "$HOOK" >/dev/null 2>&1
  local rc=$?
  if [ "$rc" -eq 0 ]; then
    PASS=$((PASS + 1))
    echo "  PASS: $desc (exit $rc)"
  else
    FAIL=$((FAIL + 1))
    echo "  FAIL: $desc (exit $rc, expected 0)"
  fi
}

echo "## LEGION_SKIP_PRE_GREP override"

LEGION_SKIP_PRE_GREP=1 assert_empty_stdout "skip override produces no stdout" <<'EOF'
{"cwd": "/tmp/test", "tool_name": "Grep", "tool_input": {"pattern": "authentication"}}
EOF

echo ""
echo "## Input validation"

assert_empty_stdout "missing cwd exits silently" <<'EOF'
{"tool_name": "Grep", "tool_input": {"pattern": "authentication"}}
EOF

assert_empty_stdout "missing tool_name exits silently" <<'EOF'
{"cwd": "/tmp/test", "tool_input": {"pattern": "authentication"}}
EOF

assert_exit_zero "empty stdin exits 0" <<'EOF'
EOF

assert_exit_zero "malformed JSON exits 0" <<'EOF'
{this is not valid json
EOF

echo ""
echo "## Grep regex-stripping"

assert_empty_stdout "regex-heavy pattern with no tokens skips injection" <<'EOF'
{"cwd": "/tmp/test", "tool_name": "Grep", "tool_input": {"pattern": "\\s+\\w+"}}
EOF

assert_empty_stdout "character class only skips injection" <<'EOF'
{"cwd": "/tmp/test", "tool_name": "Grep", "tool_input": {"pattern": "[A-Z][a-z]+"}}
EOF

assert_empty_stdout "single-token pattern skips injection" <<'EOF'
{"cwd": "/tmp/test", "tool_name": "Grep", "tool_input": {"pattern": "fn"}}
EOF

# This one passes the token filter; whether it injects depends on recall
# results, which we cannot guarantee without a populated DB. We only assert
# exit 0 (fail-open).
assert_exit_zero "multi-token semantic pattern proceeds past token filter" <<'EOF'
{"cwd": "/tmp/test", "tool_name": "Grep", "tool_input": {"pattern": "hook output format"}}
EOF

echo ""
echo "## Glob wildcard handling"

assert_empty_stdout "pure wildcard **/*.rs skips injection" <<'EOF'
{"cwd": "/tmp/test", "tool_name": "Glob", "tool_input": {"pattern": "**/*.rs"}}
EOF

assert_empty_stdout "pure wildcard *.toml skips injection" <<'EOF'
{"cwd": "/tmp/test", "tool_name": "Glob", "tool_input": {"pattern": "*.toml"}}
EOF

assert_empty_stdout "pure *** skips injection" <<'EOF'
{"cwd": "/tmp/test", "tool_name": "Glob", "tool_input": {"pattern": "***"}}
EOF

# A glob with semantic segments should proceed past the wildcard filter.
# Again we only assert exit 0 since recall results depend on the DB.
assert_exit_zero "semantic glob src/**/auth.rs proceeds past wildcard filter" <<'EOF'
{"cwd": "/tmp/test", "tool_name": "Glob", "tool_input": {"pattern": "src/**/auth.rs"}}
EOF

echo ""
echo "## Non-Grep/Glob tools exit silently"

assert_empty_stdout "Edit tool is not handled" <<'EOF'
{"cwd": "/tmp/test", "tool_name": "Edit", "tool_input": {"file_path": "/tmp/x"}}
EOF

assert_empty_stdout "Bash tool is not handled" <<'EOF'
{"cwd": "/tmp/test", "tool_name": "Bash", "tool_input": {"command": "ls"}}
EOF

echo ""
echo "---"
echo "Results: $PASS passed, $FAIL failed"
if [ "$FAIL" -gt 0 ]; then
  exit 1
fi
exit 0
