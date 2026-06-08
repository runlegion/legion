#!/bin/bash
# Test runner for the SubagentStop hook (#570).
#
# subagent-stop.sh persists a finished subagent's transcript tail as a
# domain=checkpoint reflection (tagged subagent,auto) and injects a one-line
# pointer to the parent via hookSpecificOutput.additionalContext. Every failure
# path must exit 0 (never block the parent).
#
# Run from the repo root:
#   bash plugin/hooks/test-subagent-stop.sh

set -u

PASS=0
FAIL=0

assert_contains() {
  local desc="$1" haystack="$2" needle="$3"
  if echo "$haystack" | grep -q -- "$needle"; then
    PASS=$((PASS + 1)); echo "  PASS: $desc"
  else
    FAIL=$((FAIL + 1)); echo "  FAIL: $desc" >&2
    echo "    expected to find: $needle" >&2
    echo "    in: $haystack" >&2
  fi
}

assert_not_contains() {
  local desc="$1" haystack="$2" needle="$3"
  if echo "$haystack" | grep -q -- "$needle"; then
    FAIL=$((FAIL + 1)); echo "  FAIL: $desc" >&2
    echo "    expected NOT to find: $needle" >&2
  else
    PASS=$((PASS + 1)); echo "  PASS: $desc"
  fi
}

assert_empty() {
  local desc="$1" actual="$2"
  if [ -z "$actual" ]; then
    PASS=$((PASS + 1)); echo "  PASS: $desc"
  else
    FAIL=$((FAIL + 1)); echo "  FAIL: $desc" >&2
    echo "    expected empty, got: $actual" >&2
  fi
}

WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT

mkdir -p "$WORK/plugin/bin" "$WORK/plugin/hooks"
cp plugin/hooks/subagent-stop.sh "$WORK/plugin/hooks/"

# Stub legion: logs every invocation to $LEGION_STUB_LOG, then exit 0.
cat > "$WORK/plugin/bin/legion" <<'EOF'
#!/bin/bash
if [ -n "${LEGION_STUB_LOG:-}" ]; then
  echo "$@" >> "$LEGION_STUB_LOG"
fi
exit 0
EOF
chmod +x "$WORK/plugin/bin/legion"

export CLAUDE_PLUGIN_ROOT="$WORK/plugin"
HOOK="$WORK/plugin/hooks/subagent-stop.sh"
CWD="/tmp/legion-subagent-test"
STUB_LOG="$WORK/legion-calls.log"

# A fake subagent transcript with assistant text content.
TRANSCRIPT="$WORK/agent-transcript.jsonl"
{
  echo '{"type":"assistant","message":{"content":[{"type":"text","text":"Explored the auth module and found the token refresh bug."}]}}'
  echo '{"type":"assistant","message":{"content":[{"type":"text","text":"Root cause: cookie SameSite=Strict drops the refresh on cross-site nav."}]}}'
} > "$TRANSCRIPT"

echo "==> persists a subagent checkpoint + informs the parent"
: > "$STUB_LOG"
out=$(echo "{\"cwd\":\"${CWD}\",\"agent_type\":\"Explore\",\"agent_transcript_path\":\"${TRANSCRIPT}\"}" \
  | LEGION_STUB_LOG="$STUB_LOG" bash "$HOOK")
assert_contains "reflect called with domain=checkpoint" "$(cat "$STUB_LOG")" 'reflect --repo legion-subagent-test --domain checkpoint'
assert_contains "tagged subagent,auto" "$(cat "$STUB_LOG")" 'subagent,auto'
assert_contains "checkpoint text names the agent_type" "$(cat "$STUB_LOG")" 'SUBAGENT CHECKPOINT] Explore'
assert_contains "scraped the transcript summary" "$(cat "$STUB_LOG")" 'token refresh bug'
assert_contains "parent context is additionalContext" "$out" '"hookEventName": "SubagentStop"'
assert_contains "parent pointer names recall" "$out" 'legion recall --repo legion-subagent-test --domain checkpoint'

echo "==> missing transcript: skip reflect, still inform parent, exit 0"
: > "$STUB_LOG"
out=$(echo "{\"cwd\":\"${CWD}\",\"agent_type\":\"Explore\",\"agent_transcript_path\":\"/no/such/file.jsonl\"}" \
  | LEGION_STUB_LOG="$STUB_LOG" bash "$HOOK")
assert_not_contains "no reflect call on missing transcript" "$(cat "$STUB_LOG")" 'reflect'
assert_contains "parent still informed" "$out" '"hookEventName": "SubagentStop"'

echo "==> no cwd: pass through silently"
out=$(echo '{}' | bash "$HOOK")
assert_empty "no cwd produces no output" "$out"

echo "==> missing legion binary: exit 0, no output"
out=$(echo "{\"cwd\":\"${CWD}\",\"agent_type\":\"Explore\",\"agent_transcript_path\":\"${TRANSCRIPT}\"}" \
  | CLAUDE_PLUGIN_ROOT="/no/such/plugin" bash "$HOOK")
assert_empty "missing binary produces no output" "$out"

echo
echo "==> $PASS passed, $FAIL failed"
if [ "$FAIL" -gt 0 ]; then
  exit 1
fi
exit 0
