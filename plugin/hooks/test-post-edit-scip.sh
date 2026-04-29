#!/bin/bash
# Test runner for post-edit-scip.sh.
#
# Stubs CLAUDE_PLUGIN_ROOT/bin/legion with a script that records its
# argv into a tempfile, then feeds synthetic hook JSON and verifies
# the hook spawns the indexer with the correct --file argument while
# never blocking the calling shell beyond the configured timeout.
#
# Run from the repo root:
#   bash plugin/hooks/test-post-edit-scip.sh
#
# Exits 0 on success, 1 on any failed assertion.

set -u

HOOK="${CLAUDE_PLUGIN_ROOT:-$(pwd)/plugin}/hooks/post-edit-scip.sh"
if [ ! -f "$HOOK" ]; then
  echo "FAIL: hook not found at $HOOK" >&2
  exit 1
fi

PASS=0
FAIL=0

# Build a sandboxed plugin root with a stub legion that writes its
# argv to a temp file and exits.
SANDBOX=$(mktemp -d)
trap 'rm -rf "$SANDBOX"' EXIT
mkdir -p "$SANDBOX/plugin/bin" "$SANDBOX/plugin/hooks"
cp "$(dirname "$HOOK")"/_legion-covered.sh "$SANDBOX/plugin/hooks/" 2>/dev/null || true
cp "$HOOK" "$SANDBOX/plugin/hooks/"

cat >"$SANDBOX/plugin/bin/legion" <<EOF
#!/bin/bash
# Stub legion that echoes its full argv to STUB_ARGV_FILE for assertions.
# STUB_ARGV_FILE is hardcoded into the script body because nohup'd bg
# processes do not always inherit env vars consistently across macOS
# launchd/coreutils versions.
echo "\$@" >> "$SANDBOX/argv.log"
case "\$1" in
  watch) printf 'testrepo\t/tmp\n' ;;
  stats) printf 'testrepo: 1 reflections (2026-01-01 to 2026-01-01)\n' ;;
esac
exit 0
EOF
chmod +x "$SANDBOX/plugin/bin/legion"

run_hook() {
  local file_path="$1"
  local input
  input=$(jq -n --arg fp "$file_path" --arg cwd "$SANDBOX" --arg sid "test-session" '{
    tool_input: { file_path: $fp },
    cwd: $cwd,
    session_id: $sid
  }')
  STUB_ARGV_FILE="$SANDBOX/argv.log" \
    CLAUDE_PLUGIN_ROOT="$SANDBOX/plugin" \
    XDG_CACHE_HOME="$SANDBOX/cache" \
    HOME="$SANDBOX/home" \
    LEGION_REPO="testrepo" \
    bash "$SANDBOX/plugin/hooks/post-edit-scip.sh" <<<"$input"
}

assert_argv_contains() {
  local desc="$1"
  local needle="$2"
  for _ in $(seq 1 20); do
    # `--` separates grep options from the pattern. Without it, a needle
    # that begins with `--file` looks like a malformed flag to grep.
    if grep -qF -- "$needle" "$SANDBOX/argv.log" 2>/dev/null; then
      PASS=$((PASS + 1))
      echo "  PASS: $desc"
      return 0
    fi
    sleep 0.3
  done
  FAIL=$((FAIL + 1))
  echo "  FAIL: $desc (needle '$needle' missing from argv.log)" >&2
  echo "  argv.log contents:" >&2
  cat "$SANDBOX/argv.log" 2>/dev/null >&2 || echo "  (empty)" >&2
}

assert_no_spawn() {
  local desc="$1"
  sleep 0.5
  if [ ! -s "$SANDBOX/argv.log" ]; then
    PASS=$((PASS + 1))
    echo "  PASS: $desc"
  else
    FAIL=$((FAIL + 1))
    echo "  FAIL: $desc (unexpected argv: $(cat "$SANDBOX/argv.log"))" >&2
  fi
}

reset_log() {
  rm -f "$SANDBOX/argv.log"
}

echo "Test: rust file path triggers legion index --file"
reset_log
run_hook "/tmp/sample.rs"
assert_argv_contains "spawned legion with --file argument" "--file /tmp/sample.rs"

echo "Test: non-source extension is filtered out"
reset_log
run_hook "/tmp/notes.md"
assert_no_spawn "markdown file does not trigger spawn"

echo "Test: missing file_path is silently skipped"
reset_log
input='{"tool_input": {}, "cwd": "'"$SANDBOX"'", "session_id": "s"}'
STUB_ARGV_FILE="$SANDBOX/argv.log" \
CLAUDE_PLUGIN_ROOT="$SANDBOX/plugin" \
XDG_CACHE_HOME="$SANDBOX/cache" \
HOME="$SANDBOX/home" \
bash "$SANDBOX/plugin/hooks/post-edit-scip.sh" <<<"$input"
assert_no_spawn "empty file_path -> no spawn"

echo "Test: LEGION_SKIP_POST_EDIT_SCIP=1 suppresses spawn"
reset_log
LEGION_SKIP_POST_EDIT_SCIP=1 run_hook "/tmp/sample.rs"
assert_no_spawn "skip override -> no spawn"

echo
echo "Results: $PASS passed, $FAIL failed"
if [ "$FAIL" -gt 0 ]; then
  exit 1
fi
exit 0
