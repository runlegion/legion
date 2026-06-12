#!/bin/bash
# Test runner for _legion-daemon-supervisor.sh.
#
# Uses the shared stub legion (FAKE_VERSION + FAKE_SPAWN_LOG) and a local
# `curl` shim so the test never hits a real port or spawns a real daemon.
# Asserts the spawn decision logged to the test log file matches the
# expected branch for each /health response shape.
#
# Run from anywhere:
#   bash plugin/hooks/test-daemon-supervisor.sh

set -u

# shellcheck source=tests/testutil.sh
source "$(dirname "${BASH_SOURCE[0]}")/tests/testutil.sh"

make_plugin_root _legion-daemon-supervisor.sh

mkdir -p "$WORK/scratch"
export FAKE_VERSION="9.9.9"
export FAKE_SPAWN_LOG="$WORK/scratch/spawned.log"

# Redirect /tmp/legion-hook-errors.log to a per-test path: copy the hook to
# a per-test variant and rewrite the LOG var.
HOOK_PATH="$CLAUDE_PLUGIN_ROOT/hooks/_legion-daemon-supervisor.sh"
TEST_LOG="$WORK/scratch/hook.log"
sed -i.bak "s|LOG=/tmp/legion-hook-errors.log|LOG=${TEST_LOG}|" "$HOOK_PATH"
rm -f "$HOOK_PATH.bak"

# Stub `curl` via a directory shim that appears earlier on PATH. Each test
# rewrites the stub to emit the response shape it wants.
mkdir -p "$WORK/curl-shim"
export PATH="$WORK/curl-shim:$PATH"

write_curl_stub() {
  local mode="$1"
  cat > "$WORK/curl-shim/curl" <<EOF
#!/bin/bash
case "$mode" in
  refused) exit 7 ;;
  empty) exit 0 ;;
  match) echo '{"status":"ok","version":"9.9.9","role":"serve","started_at":"2026-05-10T00:00:00Z","uptime_secs":10}' ;;
  mismatch) echo '{"status":"ok","version":"1.0.0","started_at":"2026-05-10T00:00:00Z","uptime_secs":10}' ;;
  mismatch_daemon) echo '{"status":"ok","version":"1.0.0","role":"daemon","started_at":"2026-05-10T00:00:00Z","uptime_secs":10}' ;;
  malformed) echo 'not json' ;;
esac
EOF
  chmod +x "$WORK/curl-shim/curl"
}

reset_log() {
  rm -f "$TEST_LOG" "$FAKE_SPAWN_LOG"
}

echo "==> /health unreachable -> spawn fresh"
reset_log
write_curl_stub refused
bash "$HOOK_PATH"
wait
sleep 0.3
assert_file_contains "spawned at least once" "$FAKE_SPAWN_LOG" "spawned at"
assert_file_contains "log notes started-fresh reason" "$TEST_LOG" "started fresh"

echo "==> /health empty body -> spawn fresh"
reset_log
write_curl_stub empty
bash "$HOOK_PATH"
wait
sleep 0.3
assert_file_contains "spawned at least once" "$FAKE_SPAWN_LOG" "spawned at"

echo "==> /health version match -> silent no-op"
reset_log
write_curl_stub match
bash "$HOOK_PATH"
wait
sleep 0.3
assert_file_absent "no spawn on healthy match" "$FAKE_SPAWN_LOG"
if [ ! -f "$TEST_LOG" ] || [ ! -s "$TEST_LOG" ]; then
  PASS=$((PASS + 1)); echo "  PASS: log silent on healthy match"
else
  FAIL=$((FAIL + 1)); echo "  FAIL: log unexpectedly written: $(cat "$TEST_LOG")" >&2
fi

echo "==> /health version mismatch -> replace"
reset_log
write_curl_stub mismatch
bash "$HOOK_PATH"
wait
sleep 0.3
assert_file_contains "spawned replacement" "$FAKE_SPAWN_LOG" "spawned at"
assert_file_contains "log notes replacement" "$TEST_LOG" "replaced stale v1.0.0"

echo "==> /health version mismatch with role=daemon -> daemon-restart, never a serve spawn"
reset_log
write_curl_stub mismatch_daemon
bash "$HOOK_PATH"
wait
sleep 0.3
assert_file_contains "daemon bounced in place" "$FAKE_SPAWN_LOG" "daemon-restart at"
assert_file_not_contains "no serve spawned over the daemon" "$FAKE_SPAWN_LOG" "spawned at"
assert_file_contains "log notes in-place restart" "$TEST_LOG" "restarting daemon in place"

echo "==> /health malformed JSON -> respawn"
reset_log
write_curl_stub malformed
bash "$HOOK_PATH"
wait
sleep 0.3
assert_file_contains "spawned replacement on malformed JSON" "$FAKE_SPAWN_LOG" "spawned at"
assert_file_contains "log notes malformed-health respawn" "$TEST_LOG" "malformed health response"

echo "==> LEGION_SKIP_DAEMON_SUPERVISOR=1 -> silent skip"
reset_log
write_curl_stub refused
LEGION_SKIP_DAEMON_SUPERVISOR=1 bash "$HOOK_PATH"
wait
sleep 0.3
assert_file_not_contains "skip env prevents spawn" "$FAKE_SPAWN_LOG" "spawned at"

finish_tests
