#!/bin/bash
# Test runner for _legion-daemon-supervisor.sh.
#
# Stubs `legion` and `curl` so the test never hits a real port or spawns
# a real daemon. Asserts the spawn decision logged to the test log file
# matches the expected branch for each /health response shape.
#
# Run from the repo root:
#   bash plugin/hooks/test-daemon-supervisor.sh

set -u

PASS=0
FAIL=0

assert_contains() {
  local desc="$1" file="$2" needle="$3"
  if [ -f "$file" ] && grep -q -- "$needle" "$file"; then
    PASS=$((PASS + 1))
    echo "  PASS: $desc"
  else
    FAIL=$((FAIL + 1))
    echo "  FAIL: $desc" >&2
    echo "    expected to find: $needle" >&2
    echo "    in file: $file" >&2
    [ -f "$file" ] && echo "    contents: $(cat "$file")" >&2
  fi
}

assert_no_match() {
  local desc="$1" file="$2" needle="$3"
  if [ ! -f "$file" ] || ! grep -q -- "$needle" "$file" 2>/dev/null; then
    PASS=$((PASS + 1))
    echo "  PASS: $desc"
  else
    FAIL=$((FAIL + 1))
    echo "  FAIL: $desc (unexpectedly found: $needle)" >&2
    [ -f "$file" ] && echo "    contents: $(cat "$file")" >&2
  fi
}

WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT

mkdir -p "$WORK/plugin/bin" "$WORK/plugin/hooks" "$WORK/state/legion" "$WORK/scratch"
cp plugin/hooks/_legion-daemon-supervisor.sh "$WORK/plugin/hooks/"

# Stub `legion` binary. Responds to --version and to `serve` (no-op exit 0).
cat > "$WORK/plugin/bin/legion" <<EOF
#!/bin/bash
case "\$1" in
  --version) echo "legion 9.9.9" ;;
  serve)
    # Pretend to spawn. Record into a marker the test reads.
    echo "spawned at \$(date +%s)" >> "${WORK}/scratch/spawned.log"
    ;;
esac
EOF
chmod +x "$WORK/plugin/bin/legion"

export CLAUDE_PLUGIN_ROOT="$WORK/plugin"
export XDG_STATE_HOME="$WORK/state"

# Redirect /tmp/legion-hook-errors.log to a per-test path by overriding
# the LOG var. The script uses a fixed /tmp path so we instead probe a
# scratch log via env-var substitution: copy the hook to a per-test variant
# and rewrite the LOG var.
HOOK_PATH="$WORK/plugin/hooks/_legion-daemon-supervisor.sh"
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
  match) echo '{"status":"ok","version":"9.9.9","started_at":"2026-05-10T00:00:00Z","uptime_secs":10}' ;;
  mismatch) echo '{"status":"ok","version":"1.0.0","started_at":"2026-05-10T00:00:00Z","uptime_secs":10}' ;;
  malformed) echo 'not json' ;;
esac
EOF
  chmod +x "$WORK/curl-shim/curl"
}

reset_log() {
  rm -f "$TEST_LOG" "$WORK/scratch/spawned.log"
}

echo "==> /health unreachable -> spawn fresh"
reset_log
write_curl_stub refused
bash "$HOOK_PATH"
wait
sleep 0.3
assert_contains "spawned at least once" "$WORK/scratch/spawned.log" "spawned at"
assert_contains "log notes started-fresh reason" "$TEST_LOG" "started fresh"

echo "==> /health empty body -> spawn fresh"
reset_log
write_curl_stub empty
bash "$HOOK_PATH"
wait
sleep 0.3
assert_contains "spawned at least once" "$WORK/scratch/spawned.log" "spawned at"

echo "==> /health version match -> silent no-op"
reset_log
write_curl_stub match
bash "$HOOK_PATH"
wait
sleep 0.3
if [ ! -f "$WORK/scratch/spawned.log" ]; then
  PASS=$((PASS + 1)); echo "  PASS: no spawn on healthy match"
else
  FAIL=$((FAIL + 1)); echo "  FAIL: spawned when daemon was healthy + matching" >&2
fi
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
assert_contains "spawned replacement" "$WORK/scratch/spawned.log" "spawned at"
assert_contains "log notes replacement" "$TEST_LOG" "replaced stale v1.0.0"

echo "==> /health malformed JSON -> respawn"
reset_log
write_curl_stub malformed
bash "$HOOK_PATH"
wait
sleep 0.3
assert_contains "spawned replacement on malformed JSON" "$WORK/scratch/spawned.log" "spawned at"
assert_contains "log notes malformed-health respawn" "$TEST_LOG" "malformed health response"

echo "==> LEGION_SKIP_DAEMON_SUPERVISOR=1 -> silent skip"
reset_log
write_curl_stub refused
LEGION_SKIP_DAEMON_SUPERVISOR=1 bash "$HOOK_PATH"
wait
sleep 0.3
assert_no_match "skip env prevents spawn" "$WORK/scratch/spawned.log" "spawned at"

echo
echo "==> $PASS passed, $FAIL failed"
if [ "$FAIL" -gt 0 ]; then
  exit 1
fi
exit 0
