#!/bin/bash
# Test runner for stop.sh's pending-replies gate (#1020).
#
# HOOK OUTPUT DOCTRINE (Sean, 2026-08-27, reflection 01a0421f-3bb1-7a91-
# a2d4-1587442dbd36): a Stop that ends the turn while a directed
# wake-worthy signal sits unanswered is a silent ghost -- the miss that
# prompted this gate was a 40-minute-unanswered rfc (01a04213) whose only
# surfacing was a hook drain note easy to read past. This gate re-checks
# `legion pending-replies --repo $REPO --directed` (excluding
# @all/@everyone broadcasts, which no single reply can retire -- review
# finding HIGH 1) at Stop time and hard-blocks (decision:block) whenever
# it is non-empty -- same LEGION_SKIP_STOP_BLOCK=1 bypass and 8-block
# harness backstop as the delegated-liveness gate (#778), and it ignores
# stop_hook_active for the same reason that gate does: the obligation
# must keep blocking until it is actually answered, not until one
# continuation turn passes. It is also coverage-gated via
# `legion_hook_covered` (review finding HIGH 2): in a git worktree $REPO
# is the worktree directory name, not the parent repo watch.toml knows,
# so an uncovered cwd must not be judged against a name it is not.
#
# The other two Stop gates (delegated-liveness, reflection nudge) are
# covered in test-stop-task-block.sh; this file is scoped to the new
# pending-replies gate alone, following test-daemon-supervisor.sh's
# per-hook layout.
#
# Run from anywhere:
#   bash plugin/hooks/test-stop.sh

set -u

# shellcheck source=tests/testutil.sh
source "$(dirname "${BASH_SOURCE[0]}")/tests/testutil.sh"

make_plugin_root stop.sh

STOP_HOOK="$CLAUDE_PLUGIN_ROOT/hooks/stop.sh"
CWD="/tmp/legion-test-1020"
CWD_HASH=$(legion_hash_str "$CWD")
# stop.sh's reflection-nudge gate also touches these per-CWD markers on a
# clean-board fallthrough; keep them out of this run so the pending-replies
# gate is the only thing under test. make_plugin_root already trapped $WORK
# for cleanup -- extend, don't replace, so that cleanup still happens.
trap 'rm -rf "$WORK" "/tmp/legion-work-${CWD_HASH}" "/tmp/legion-reflected-${CWD_HASH}"' EXIT

# The gate is coverage-gated (#353, HIGH 2): $REPO resolves to
# basename($CWD) ("legion-test-1020" for our $CWD above), and the coverage
# probe treats a repo present in `watch list` as covered. Without this,
# every test below would silently no-op the gate rather than exercise it.
export FAKE_WATCH="legion-test-1020 /tmp/legion-test-1020"
export LEGION_STUB_LOG="$WORK/stub.log"

echo "==> non-empty legion pending-replies blocks the Stop, naming the signal, with true framing and a bypass footer (#1020)"
: > "$LEGION_STUB_LOG"
out=$(echo "{\"cwd\":\"${CWD}\",\"session_id\":\"pending-1\"}" \
  | FAKE_PENDING_REPLIES="REQUIRES A REPLY -- rafters asked something urgent (id: sig-1)" \
    bash "$STOP_HOOK")
assert_contains "decision:block present" "$out" '"decision": "block"'
assert_contains "names the pending signal" "$out" "rafters asked something urgent"
assert_contains "opens with true framing, not the raw wake-prompt preamble" "$out" \
  "Open directed asks for legion-test-1020; the turn cannot end until each is answered"
assert_not_contains "does not carry the misleading auto-woken preamble" "$out" \
  "You were auto-woken by legion watch"
assert_contains "tells the agent how to reply" "$out" "legion signal --repo legion-test-1020 --to <author> --verb answer"
assert_contains "warns that --to all retires nothing" "$out" "a reply --to all retires nothing"
assert_contains "carries the bypass footer" "$out" "LEGION_SKIP_STOP_BLOCK=1"
assert_file_contains "queries pending-replies with --directed" "$LEGION_STUB_LOG" \
  "pending-replies --repo legion-test-1020 --directed"

echo "==> empty legion pending-replies does not block on this gate"
rm -f "/tmp/legion-reflected-${CWD_HASH}"
out=$(echo "{\"cwd\":\"${CWD}\",\"session_id\":\"pending-2\"}" | bash "$STOP_HOOK")
assert_not_contains "no pending replies -> no decision:block" "$out" '"decision": "block"'
assert_not_contains "no pending replies -> no block from this gate" "$out" "REQUIRES A REPLY"

echo "==> LEGION_SKIP_STOP_BLOCK=1 bypasses the pending-replies gate too, and exits 0"
rm -f "/tmp/legion-reflected-${CWD_HASH}"
out=$(echo "{\"cwd\":\"${CWD}\",\"session_id\":\"pending-3\"}" \
  | FAKE_PENDING_REPLIES="REQUIRES A REPLY -- should not surface" \
    LEGION_SKIP_STOP_BLOCK=1 \
    bash "$STOP_HOOK")
rc=$?
assert_empty "bypass produces no output" "$out"
assert_rc "bypass exits 0" 0 "$rc"

echo "==> LEGION_SKIP_STOP_BLOCK=1 records the bypass via telemetry (audited escape) and exits 0"
rm -f "/tmp/legion-reflected-${CWD_HASH}"
MARKER_LOG="$WORK/bypass.log"
out=$(echo "{\"cwd\":\"${CWD}\",\"session_id\":\"pending-4\"}" \
  | FAKE_PENDING_REPLIES="REQUIRES A REPLY -- should not surface" \
    LEGION_SKIP_STOP_BLOCK=1 \
    LEGION_TEST_MARKER="$MARKER_LOG" \
    bash "$STOP_HOOK")
rc=$?
assert_empty "bypass still produces no output with telemetry wired" "$out"
assert_rc "bypass with telemetry wired still exits 0" 0 "$rc"
assert_file_contains "bypass recorded via telemetry record-bypass" "$MARKER_LOG" "record-bypass"

echo "==> pending-replies gate ignores stop_hook_active -- keeps blocking (#1020, mirrors gate 1)"
rm -f "/tmp/legion-reflected-${CWD_HASH}"
out=$(echo "{\"cwd\":\"${CWD}\",\"session_id\":\"pending-5\",\"stop_hook_active\":true}" \
  | FAKE_PENDING_REPLIES="REQUIRES A REPLY -- still open" \
    bash "$STOP_HOOK")
assert_contains "still blocks even when stop_hook_active is true" "$out" '"decision": "block"'

echo "==> watch-pty (#492) skips the pending-replies gate along with the others"
rm -f "/tmp/legion-reflected-${CWD_HASH}"
out=$(echo "{\"cwd\":\"${CWD}\",\"session_id\":\"pending-6\"}" \
  | FAKE_PENDING_REPLIES="REQUIRES A REPLY -- should not surface" \
    LEGION_SPAWN_SOURCE=watch-pty \
    bash "$STOP_HOOK")
assert_empty "watch-pty bypass produces no output" "$out"

echo "==> the gate fires even with no file-editing work this session (no WORK_MARKER)"
# Regression guard for gate ordering: the pending-replies gate must run
# BEFORE the reflection nudge's WORK_MARKER check, or a Stop that touched
# no files would fall through this gate entirely on an open ask.
rm -f "/tmp/legion-reflected-${CWD_HASH}" "/tmp/legion-work-${CWD_HASH}"
out=$(echo "{\"cwd\":\"${CWD}\",\"session_id\":\"pending-7\"}" \
  | FAKE_PENDING_REPLIES="REQUIRES A REPLY -- no tool calls happened yet" \
    bash "$STOP_HOOK")
assert_contains "blocks even with no WORK_MARKER present" "$out" '"decision": "block"'

echo "==> an uncovered cwd does not block, even with pending replies fixtured (#1020 review, HIGH 2)"
# A different cwd, absent from FAKE_WATCH and with no reflections --
# legion_hook_covered must resolve it as not-covered, and the gate must
# exit cleanly before ever querying pending-replies for a repo name that
# is really just this worktree's directory, not a real repo identity.
rm -f "/tmp/legion-reflected-$(legion_hash_str "/tmp/uncovered-repo-1020")"
: > "$LEGION_STUB_LOG"
out=$(echo "{\"cwd\":\"/tmp/uncovered-repo-1020\",\"session_id\":\"pending-8\"}" \
  | FAKE_PENDING_REPLIES="REQUIRES A REPLY -- should not surface" \
    bash "$STOP_HOOK")
assert_not_contains "uncovered cwd -> no decision:block" "$out" '"decision": "block"'
assert_not_contains "uncovered cwd never calls pending-replies" "$(cat "$LEGION_STUB_LOG")" "pending-replies"
rm -f "/tmp/legion-reflected-$(legion_hash_str "/tmp/uncovered-repo-1020")"

echo "==> a broken legion binary never blocks the Stop (fails open, #1020)"
rm -f "/tmp/legion-reflected-${CWD_HASH}"
out=$(echo "{\"cwd\":\"${CWD}\",\"session_id\":\"pending-9\"}" \
  | FAKE_PENDING_REPLIES="REQUIRES A REPLY -- should not surface" \
    FAKE_BROKEN=1 \
    bash "$STOP_HOOK")
assert_not_contains "broken legion -> no decision:block from this gate" "$out" '"decision": "block"'

echo "==> a multi-line pending-replies body renders intact inside the block (#1020)"
rm -f "/tmp/legion-reflected-${CWD_HASH}"
MULTI="REQUIRES A REPLY -- these are directed questions and requests.

- [from rafters] question: which lane owns retries (id: sig-1)
- [from smugglr] request: review the tokenizer spec (id: sig-2)"
out=$(echo "{\"cwd\":\"${CWD}\",\"session_id\":\"pending-10\"}" \
  | FAKE_PENDING_REPLIES="$MULTI" \
    bash "$STOP_HOOK")
assert_contains "decision:block present for a multi-line body" "$out" '"decision": "block"'
assert_contains "first signal entry intact" "$out" "which lane owns retries"
assert_contains "second signal entry intact" "$out" "review the tokenizer spec"

finish_tests
