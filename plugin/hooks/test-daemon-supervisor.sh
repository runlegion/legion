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
# Local build id (#698): the supervisor reads it from --version's "(build <id>)"
# suffix and compares against /health's build_id on a version match.
export FAKE_BUILD="localbuild"
export FAKE_SPAWN_LOG="$WORK/scratch/spawned.log"
# #997: records every stub-legion invocation's full argv, so a test can
# assert the exact subcommand used (`daemon-spawn` vs `serve`) rather than
# relying only on FAKE_SPAWN_LOG's shared "spawned at" text.
export LEGION_STUB_LOG="$WORK/scratch/stub-argv.log"

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
  match_build_same) echo '{"status":"ok","version":"9.9.9","build_id":"localbuild","role":"daemon","started_at":"2026-05-10T00:00:00Z","uptime_secs":10}' ;;
  match_build_drift_daemon) echo '{"status":"ok","version":"9.9.9","build_id":"oldbuild","role":"daemon","started_at":"2026-05-10T00:00:00Z","uptime_secs":10}' ;;
  match_build_drift_serve) echo '{"status":"ok","version":"9.9.9","build_id":"oldbuild","role":"serve","started_at":"2026-05-10T00:00:00Z","uptime_secs":10}' ;;
  match_build_unknown) echo '{"status":"ok","version":"9.9.9","build_id":"unknown","role":"daemon","started_at":"2026-05-10T00:00:00Z","uptime_secs":10}' ;;
  mismatch) echo '{"status":"ok","version":"1.0.0","started_at":"2026-05-10T00:00:00Z","uptime_secs":10}' ;;
  mismatch_daemon) echo '{"status":"ok","version":"1.0.0","role":"daemon","started_at":"2026-05-10T00:00:00Z","uptime_secs":10}' ;;
  # #997 acceptance criterion: a malformed-role (empty string, as a
  # pre-#613 binary or a transitional build might report) response still
  # goes through daemon-restart, never a pidfile kill + serve.
  mismatch_empty_role) echo '{"version":"0.0.1","role":""}' ;;
  malformed) echo 'not json' ;;
esac
EOF
  chmod +x "$WORK/curl-shim/curl"
}

reset_log() {
  rm -f "$TEST_LOG" "$FAKE_SPAWN_LOG" "$LEGION_STUB_LOG"
}

echo "==> /health unreachable -> daemon-spawn cold start, never serve (#997)"
reset_log
write_curl_stub refused
bash "$HOOK_PATH"
wait
sleep 0.3
assert_file_contains "spawned at least once" "$FAKE_SPAWN_LOG" "spawned at"
assert_file_contains "log notes started-fresh reason" "$TEST_LOG" "started fresh"
assert_file_contains "cold start calls daemon-spawn" "$LEGION_STUB_LOG" "daemon-spawn"
assert_file_not_contains "cold start never calls serve" "$LEGION_STUB_LOG" "^serve$"

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

echo "==> /health version match + build_id absent -> silent no-op (fallback)"
reset_log
write_curl_stub match
bash "$HOOK_PATH"
wait
sleep 0.3
assert_file_absent "no spawn when daemon build_id is absent" "$FAKE_SPAWN_LOG"

echo "==> /health version match + same build_id -> silent no-op"
reset_log
write_curl_stub match_build_same
bash "$HOOK_PATH"
wait
sleep 0.3
assert_file_absent "no spawn when build ids match" "$FAKE_SPAWN_LOG"
if [ ! -f "$TEST_LOG" ] || [ ! -s "$TEST_LOG" ]; then
  PASS=$((PASS + 1)); echo "  PASS: log silent on same-build match"
else
  FAIL=$((FAIL + 1)); echo "  FAIL: log unexpectedly written: $(cat "$TEST_LOG")" >&2
fi

echo "==> /health version match + build drift + role=daemon -> daemon-restart (#698)"
reset_log
write_curl_stub match_build_drift_daemon
bash "$HOOK_PATH"
wait
sleep 0.3
assert_file_contains "daemon bounced on build drift" "$FAKE_SPAWN_LOG" "daemon-restart at"
assert_file_not_contains "no serve spawned over the daemon" "$FAKE_SPAWN_LOG" "spawned at"
assert_file_contains "log notes build drift" "$TEST_LOG" "daemon build oldbuild != local build localbuild"

echo "==> /health version match + build drift + role=serve -> daemon-restart, not a serve spawn (#997)"
reset_log
write_curl_stub match_build_drift_serve
bash "$HOOK_PATH"
wait
sleep 0.3
assert_file_contains "daemon-restart used for a legacy serve role" "$FAKE_SPAWN_LOG" "daemon-restart at"
assert_file_not_contains "no cold-start spawn over an answering serve" "$FAKE_SPAWN_LOG" "spawned at"
assert_file_contains "log notes build drift" "$TEST_LOG" "daemon build oldbuild != local build localbuild"

echo "==> /health version match + build_id unknown -> silent no-op (indeterminate)"
reset_log
write_curl_stub match_build_unknown
bash "$HOOK_PATH"
wait
sleep 0.3
assert_file_absent "no spawn when daemon build_id is unknown" "$FAKE_SPAWN_LOG"

echo "==> /health version mismatch, no role (pre-#613 binary) -> daemon-restart, not a serve spawn (#997)"
reset_log
write_curl_stub mismatch
bash "$HOOK_PATH"
wait
sleep 0.3
assert_file_contains "daemon-restart used even with no role reported" "$FAKE_SPAWN_LOG" "daemon-restart at"
assert_file_not_contains "no cold-start spawn on version drift" "$FAKE_SPAWN_LOG" "spawned at"
assert_file_contains "log notes replacement" "$TEST_LOG" "daemon v1.0.0 != local v9.9.9"

echo "==> /health version mismatch with role=daemon -> daemon-restart, never a serve spawn"
reset_log
write_curl_stub mismatch_daemon
bash "$HOOK_PATH"
wait
sleep 0.3
assert_file_contains "daemon bounced in place" "$FAKE_SPAWN_LOG" "daemon-restart at"
assert_file_not_contains "no serve spawned over the daemon" "$FAKE_SPAWN_LOG" "spawned at"
assert_file_contains "log notes in-place restart" "$TEST_LOG" "restarting via daemon-restart"

echo "==> /health {version:0.0.1, role:\"\"} vs local 9.9.9 -> daemon-restart, not a kill+serve (#997 AC2)"
reset_log
write_curl_stub mismatch_empty_role
bash "$HOOK_PATH"
wait
sleep 0.3
assert_file_contains "daemon-restart used for an empty-role response" "$FAKE_SPAWN_LOG" "daemon-restart at"
assert_file_not_contains "no cold-start spawn for an empty-role response" "$FAKE_SPAWN_LOG" "spawned at"
assert_file_not_contains "no serve invocation logged for an empty-role response" "$LEGION_STUB_LOG" "^serve$"

echo "==> /health malformed JSON -> daemon-restart, not a serve spawn (#997)"
reset_log
write_curl_stub malformed
bash "$HOOK_PATH"
wait
sleep 0.3
assert_file_contains "daemon-restart used on malformed health" "$FAKE_SPAWN_LOG" "daemon-restart at"
assert_file_not_contains "no cold-start spawn on malformed health" "$FAKE_SPAWN_LOG" "spawned at"
assert_file_contains "log notes malformed-health respawn" "$TEST_LOG" "malformed health response"

echo "==> LEGION_SKIP_DAEMON_SUPERVISOR=1 -> silent skip"
reset_log
write_curl_stub refused
LEGION_SKIP_DAEMON_SUPERVISOR=1 bash "$HOOK_PATH"
wait
sleep 0.3
assert_file_not_contains "skip env prevents spawn" "$FAKE_SPAWN_LOG" "spawned at"

finish_tests
