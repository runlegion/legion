#!/bin/bash
# Test runner for the SubagentStop hook (#570).
#
# subagent-stop.sh persists a finished subagent's transcript tail as a
# domain=checkpoint reflection (tagged subagent,auto) and injects a one-line
# pointer to the parent via hookSpecificOutput.additionalContext. Every failure
# path must exit 0 (never block the parent).
#
# Run from anywhere:
#   bash plugin/hooks/test-subagent-stop.sh

set -u

# shellcheck source=tests/testutil.sh
source "$(dirname "${BASH_SOURCE[0]}")/tests/testutil.sh"

make_plugin_root subagent-stop.sh

# Route the hook's dedup markers (#584) into $WORK so they are cleaned on EXIT
# and cannot leak across test runs in the real /tmp.
export TMPDIR="$WORK"

HOOK="$CLAUDE_PLUGIN_ROOT/hooks/subagent-stop.sh"
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

echo "==> missing plugin root: exit 0, no output (fail-open source)"
out=$(echo "{\"cwd\":\"${CWD}\",\"agent_type\":\"Explore\",\"agent_transcript_path\":\"${TRANSCRIPT}\"}" \
  | CLAUDE_PLUGIN_ROOT="/no/such/plugin" bash "$HOOK")
assert_empty "missing plugin root produces no output" "$out"

echo "==> missing legion binary: exit 0, no output"
out=$(echo "{\"cwd\":\"${CWD}\",\"agent_type\":\"Explore\",\"agent_transcript_path\":\"${TRANSCRIPT}\"}" \
  | LEGION_BIN="/no/such/legion" bash "$HOOK")
assert_empty "missing binary produces no output" "$out"

echo "==> re-delivered SubagentStop is deduped: second fire silent, no double reflect (#584)"
DEDUP_TRANSCRIPT="$WORK/dedup-transcript.jsonl"
echo '{"type":"assistant","message":{"content":[{"type":"text","text":"Deduped subagent run summary."}]}}' > "$DEDUP_TRANSCRIPT"
: > "$STUB_LOG"
dedup_in="{\"cwd\":\"${CWD}\",\"agent_type\":\"Explore\",\"agent_transcript_path\":\"${DEDUP_TRANSCRIPT}\",\"session_id\":\"sess-1\"}"
out1=$(echo "$dedup_in" | LEGION_STUB_LOG="$STUB_LOG" bash "$HOOK")
out2=$(echo "$dedup_in" | LEGION_STUB_LOG="$STUB_LOG" bash "$HOOK")
assert_contains "first fire informs the parent" "$out1" '"hookEventName": "SubagentStop"'
assert_empty "second (re-delivered) fire is silent" "$out2"
reflect_count=$(grep -c 'reflect' "$STUB_LOG" 2>/dev/null || echo 0)
assert_eq "reflect called exactly once across two fires" "$reflect_count" "1"

echo "==> stop_hook_active=true short-circuits (loop guard, #584)"
: > "$STUB_LOG"
out=$(echo "{\"cwd\":\"${CWD}\",\"agent_type\":\"Explore\",\"agent_transcript_path\":\"${WORK}/guard.jsonl\",\"stop_hook_active\":true}" \
  | LEGION_STUB_LOG="$STUB_LOG" bash "$HOOK")
assert_empty "stop_hook_active produces no output" "$out"
assert_not_contains "no reflect when stop_hook_active" "$(cat "$STUB_LOG")" 'reflect'

finish_tests
