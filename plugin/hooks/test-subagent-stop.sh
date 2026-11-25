#!/bin/bash
# Test runner for the SubagentStop hook (#570, #1441).
#
# subagent-stop.sh injects a nudge via hookSpecificOutput.additionalContext
# that continues the SUBAGENT's own turn. That continuation message is what
# the Task tool actually returns to the parent, so the nudge must order the
# subagent to restate its complete deliverable (#752) -- never a bare
# acknowledgment that references a prior message. It stores no reflection
# (#1441). Every failure path must exit 0 (never block the parent).
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

echo "==> stores nothing, nudges the subagent's own turn"
: > "$STUB_LOG"
out=$(echo "{\"cwd\":\"${CWD}\",\"agent_type\":\"Explore\",\"agent_transcript_path\":\"${TRANSCRIPT}\"}" \
  | LEGION_STUB_LOG="$STUB_LOG" bash "$HOOK")
assert_not_contains "no reflect call" "$(cat "$STUB_LOG")" 'reflect'
assert_contains "nudge is additionalContext continuing the subagent's turn" "$out" '"hookEventName": "SubagentStop"'
assert_not_contains "nudge names no checkpoint recall" "$out" 'domain checkpoint'
assert_contains "prompt orders a full restatement" "$out" 'RESTATE your complete deliverable in full'
assert_contains "prompt names the next message as the only channel" "$out" 'Your NEXT message is the ONLY message the orchestrator that spawned you will ever receive'
# The needle's ".." stand in for the JSON-escaped \" quotes jq wraps each
# phrase in (grep BRE has no lookaround, so a literal quote is unreliable
# across escaped/unescaped output; "." as a single-char wildcard is not).
assert_contains "prompt forbids referencing a prior message" "$out" 'Never write ..see above.., ..already delivered.., or any other reference to a prior message'
assert_contains "restatement is ordered before the ban on pointing back" "$out" 'exactly as if reporting it for the first time.*Never write'

echo "==> missing transcript: still nudge the subagent, exit 0"
: > "$STUB_LOG"
out=$(echo "{\"cwd\":\"${CWD}\",\"agent_type\":\"Explore\",\"agent_transcript_path\":\"/no/such/file.jsonl\"}" \
  | LEGION_STUB_LOG="$STUB_LOG" bash "$HOOK")
assert_not_contains "no reflect call on missing transcript" "$(cat "$STUB_LOG")" 'reflect'
assert_contains "subagent still nudged" "$out" '"hookEventName": "SubagentStop"'

echo "==> no cwd: pass through silently"
out=$(echo '{}' | bash "$HOOK")
assert_empty "no cwd produces no output" "$out"

echo "==> missing plugin root: exit 0, no output (fail-open source)"
out=$(echo "{\"cwd\":\"${CWD}\",\"agent_type\":\"Explore\",\"agent_transcript_path\":\"${TRANSCRIPT}\"}" \
  | CLAUDE_PLUGIN_ROOT="/no/such/plugin" bash "$HOOK")
assert_empty "missing plugin root produces no output" "$out"

echo "==> missing legion binary: the nudge needs no binary, still nudges"
out=$(echo "{\"cwd\":\"${CWD}\",\"agent_type\":\"Explore\",\"agent_transcript_path\":\"${WORK}/nobin.jsonl\"}" \
  | LEGION_BIN="/no/such/legion" bash "$HOOK")
assert_contains "subagent nudged without a binary" "$out" '"hookEventName": "SubagentStop"'

echo "==> re-delivered SubagentStop is deduped: second fire silent (#584)"
DEDUP_TRANSCRIPT="$WORK/dedup-transcript.jsonl"
echo '{"type":"assistant","message":{"content":[{"type":"text","text":"Deduped subagent run summary."}]}}' > "$DEDUP_TRANSCRIPT"
: > "$STUB_LOG"
dedup_in="{\"cwd\":\"${CWD}\",\"agent_type\":\"Explore\",\"agent_transcript_path\":\"${DEDUP_TRANSCRIPT}\",\"session_id\":\"sess-1\"}"
out1=$(echo "$dedup_in" | LEGION_STUB_LOG="$STUB_LOG" bash "$HOOK")
out2=$(echo "$dedup_in" | LEGION_STUB_LOG="$STUB_LOG" bash "$HOOK")
assert_contains "first fire nudges the subagent" "$out1" '"hookEventName": "SubagentStop"'
assert_empty "second (re-delivered) fire is silent" "$out2"
assert_not_contains "no reflect across two fires" "$(cat "$STUB_LOG")" 'reflect'

echo "==> stop_hook_active=true short-circuits (loop guard, #584)"
: > "$STUB_LOG"
out=$(echo "{\"cwd\":\"${CWD}\",\"agent_type\":\"Explore\",\"agent_transcript_path\":\"${WORK}/guard.jsonl\",\"stop_hook_active\":true}" \
  | LEGION_STUB_LOG="$STUB_LOG" bash "$HOOK")
assert_empty "stop_hook_active produces no output" "$out"
assert_not_contains "no reflect when stop_hook_active" "$(cat "$STUB_LOG")" 'reflect'

finish_tests
