#!/bin/bash
# Test runner for the uncertainty engine hooks (#358).
#
# Exercises:
# - uncertainty-emit-on-task.sh: PostToolUse on TaskCreate writes a row to
#   the uncertainty-tasks-<session>.jsonl mapping file when emit succeeds.
# - uncertainty-witness-on-completion.sh: PostToolUse on TaskUpdate where
#   status=completed reads the mapping and fires witness with placeholder
#   correctness.
#
# Both hooks are non-blocking: missing jq, missing session_id, malformed
# tool input, or emit/witness CLI failures all log and exit 0 silently.
#
# Run from the repo root:
#   bash plugin/hooks/test-uncertainty-hooks.sh

set -u

PASS=0
FAIL=0

assert_eq() {
  local desc="$1" actual="$2" expected="$3"
  if [ "$actual" = "$expected" ]; then
    PASS=$((PASS + 1))
    echo "  PASS: $desc"
  else
    FAIL=$((FAIL + 1))
    echo "  FAIL: $desc" >&2
    echo "    expected: $expected" >&2
    echo "    actual:   $actual" >&2
  fi
}

assert_file_contains() {
  local desc="$1" file="$2" needle="$3"
  if [ -f "$file" ] && grep -q -- "$needle" "$file"; then
    PASS=$((PASS + 1))
    echo "  PASS: $desc"
  else
    FAIL=$((FAIL + 1))
    echo "  FAIL: $desc" >&2
    echo "    expected $file to contain: $needle" >&2
    [ -f "$file" ] && echo "    actual: $(cat "$file")" >&2
  fi
}

assert_file_absent() {
  local desc="$1" file="$2"
  if [ ! -f "$file" ]; then
    PASS=$((PASS + 1))
    echo "  PASS: $desc"
  else
    FAIL=$((FAIL + 1))
    echo "  FAIL: $desc" >&2
    echo "    expected $file to be absent" >&2
  fi
}

WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT

mkdir -p "$WORK/plugin/bin" "$WORK/plugin/hooks" "$WORK/state/legion"
cp plugin/hooks/uncertainty-emit-on-task.sh "$WORK/plugin/hooks/"
cp plugin/hooks/uncertainty-witness-on-completion.sh "$WORK/plugin/hooks/"
mkdir -p "$WORK/plugin/hooks/lib"
cp plugin/hooks/lib/prelude.sh plugin/hooks/lib/emit.sh "$WORK/plugin/hooks/lib/"

# Stub legion binary: emit returns a fixed prediction id, witness logs
# its call to a side-effect file so we can assert it ran.
cat > "$WORK/plugin/bin/legion" <<EOF
#!/bin/bash
case "\$1" in
  uncertainty)
    case "\$2" in
      emit)
        echo '{"id":"pred-fixed-1","orphan_after":"2026-06-01T00:00:00Z"}'
        ;;
      witness)
        echo "\$@" >> "$WORK/state/legion/witness-calls.log"
        ;;
    esac
    ;;
esac
exit 0
EOF
chmod +x "$WORK/plugin/bin/legion"

# Hooks resolve the binary via the prelude: plugin-root copy first.
export CLAUDE_PLUGIN_ROOT="$WORK/plugin"
export XDG_STATE_HOME="$WORK/state"

EMIT_HOOK="$WORK/plugin/hooks/uncertainty-emit-on-task.sh"
WITNESS_HOOK="$WORK/plugin/hooks/uncertainty-witness-on-completion.sh"

SESSION="test-session-358"
MAP_LOG="$WORK/state/legion/uncertainty-tasks-${SESSION}.jsonl"
WITNESS_LOG="$WORK/state/legion/witness-calls.log"

echo "==> emit hook writes mapping on TaskCreate"
echo "{\"session_id\":\"${SESSION}\",\"tool_name\":\"TaskCreate\",\"tool_input\":{\"subject\":\"Port uncertainty engine\"},\"tool_response\":{\"id\":\"task-001\"}}" | bash "$EMIT_HOOK" >/dev/null 2>&1
assert_eq "emit hook exit code is 0 on happy path" "$?" "0"
assert_file_contains "mapping file has task-001 -> pred-fixed-1" "$MAP_LOG" '"task_id":"task-001"'
assert_file_contains "mapping file has prediction_id" "$MAP_LOG" '"prediction_id":"pred-fixed-1"'

echo "==> emit hook skips non-TaskCreate tools"
rm -f "$WORK/state/legion/uncertainty-tasks-skip.jsonl"
echo "{\"session_id\":\"skip\",\"tool_name\":\"Bash\",\"tool_input\":{},\"tool_response\":{}}" | bash "$EMIT_HOOK"
assert_eq "non-TaskCreate exit code is 0" "$?" "0"
assert_file_absent "no mapping file for non-TaskCreate" "$WORK/state/legion/uncertainty-tasks-skip.jsonl"

echo "==> emit hook skips when session_id is missing"
echo "{\"tool_name\":\"TaskCreate\",\"tool_input\":{\"subject\":\"x\"},\"tool_response\":{\"id\":\"task-002\"}}" | bash "$EMIT_HOOK"
assert_eq "missing session_id exit code is 0" "$?" "0"

echo "==> emit hook skips when task_id is missing"
echo "{\"session_id\":\"${SESSION}\",\"tool_name\":\"TaskCreate\",\"tool_input\":{\"subject\":\"x\"},\"tool_response\":{}}" | bash "$EMIT_HOOK"
assert_eq "missing task_id exit code is 0" "$?" "0"

echo "==> emit hook is non-blocking on legion stub failure"
# Replace stub with one that exits non-zero on emit.
cat > "$WORK/plugin/bin/legion" <<'EOF'
#!/bin/bash
exit 1
EOF
chmod +x "$WORK/plugin/bin/legion"
echo "{\"session_id\":\"${SESSION}\",\"tool_name\":\"TaskCreate\",\"tool_input\":{\"subject\":\"fail-shape\"},\"tool_response\":{\"id\":\"task-003\"}}" | bash "$EMIT_HOOK"
assert_eq "emit failure still exits 0" "$?" "0"

# Restore happy-path stub for witness tests.
cat > "$WORK/plugin/bin/legion" <<EOF
#!/bin/bash
case "\$1" in
  uncertainty)
    case "\$2" in
      emit)
        echo '{"id":"pred-fixed-1","orphan_after":"2026-06-01T00:00:00Z"}'
        ;;
      witness)
        echo "\$@" >> "$WITNESS_LOG"
        ;;
    esac
    ;;
esac
exit 0
EOF
chmod +x "$WORK/plugin/bin/legion"

echo "==> witness hook fires on TaskUpdate completed"
rm -f "$WITNESS_LOG"
echo "{\"session_id\":\"${SESSION}\",\"tool_name\":\"TaskUpdate\",\"tool_input\":{\"task_id\":\"task-001\",\"status\":\"completed\"}}" | bash "$WITNESS_HOOK"
assert_eq "witness hook exit code is 0" "$?" "0"
assert_file_contains "witness was called with the prediction id" "$WITNESS_LOG" 'pred-fixed-1'
assert_file_contains "witness uses placeholder shipped label" "$WITNESS_LOG" '--outcome-label shipped'
assert_file_contains "witness uses correctness 1.0" "$WITNESS_LOG" '--outcome-correctness 1.0'

echo "==> witness hook skips on non-completed status"
rm -f "$WITNESS_LOG"
echo "{\"session_id\":\"${SESSION}\",\"tool_name\":\"TaskUpdate\",\"tool_input\":{\"task_id\":\"task-001\",\"status\":\"in_progress\"}}" | bash "$WITNESS_HOOK"
assert_eq "non-completed status exit code is 0" "$?" "0"
assert_file_absent "witness not called on in_progress" "$WITNESS_LOG"

echo "==> witness hook skips when mapping log is absent"
rm -f "$WITNESS_LOG"
echo "{\"session_id\":\"missing\",\"tool_name\":\"TaskUpdate\",\"tool_input\":{\"task_id\":\"task-999\",\"status\":\"completed\"}}" | bash "$WITNESS_HOOK"
assert_eq "missing mapping exit code is 0" "$?" "0"
assert_file_absent "witness not called when mapping is absent" "$WITNESS_LOG"

echo "==> witness hook skips when task_id is not in the mapping"
rm -f "$WITNESS_LOG"
echo "{\"session_id\":\"${SESSION}\",\"tool_name\":\"TaskUpdate\",\"tool_input\":{\"task_id\":\"task-not-in-map\",\"status\":\"completed\"}}" | bash "$WITNESS_HOOK"
assert_eq "unknown task_id exit code is 0" "$?" "0"
assert_file_absent "witness not called for unknown task_id" "$WITNESS_LOG"

echo "==> LEGION_SKIP_UNCERTAINTY=1 disables both hooks"
rm -f "$WITNESS_LOG"
SKIP_MAP="$WORK/state/legion/uncertainty-tasks-skip-session.jsonl"
echo "{\"session_id\":\"skip-session\",\"tool_name\":\"TaskCreate\",\"tool_input\":{\"subject\":\"x\"},\"tool_response\":{\"id\":\"task-skip\"}}" | LEGION_SKIP_UNCERTAINTY=1 bash "$EMIT_HOOK"
echo "{\"session_id\":\"${SESSION}\",\"tool_name\":\"TaskUpdate\",\"tool_input\":{\"task_id\":\"task-001\",\"status\":\"completed\"}}" | LEGION_SKIP_UNCERTAINTY=1 bash "$WITNESS_HOOK"
assert_file_absent "skip flag suppresses emit mapping write" "$SKIP_MAP"
assert_file_absent "skip flag suppresses witness call" "$WITNESS_LOG"

echo
echo "PASS: $PASS"
echo "FAIL: $FAIL"
[ "$FAIL" -eq 0 ] || exit 1
