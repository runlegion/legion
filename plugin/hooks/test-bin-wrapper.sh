#!/bin/bash
# Test runner for the bin/legion self-heal: a mid-session plugin update
# leaves the binary behind, because setup-binary.sh runs only on
# SessionStart "startup" and /reload-plugins fires no hook.
#
# Covers: a current binary runs with no setup call and primes the version
# stamp; a valid stamp skips the --version probe; an older binary is updated
# once, the daemon restarted, and the command's stdout kept clean; a failed
# update warns, runs the old binary and backs off ten minutes; an older plugin copy never pulls the
# binary down; a held lock skips the update and a stale one is cleared;
# LEGION_NO_DAEMON and LEGION_SELF_HEAL=0 are honoured.
#
# Run from anywhere:
#   bash plugin/hooks/test-bin-wrapper.sh

set -u

# shellcheck source=tests/testutil.sh
source "$(dirname "${BASH_SOURCE[0]}")/tests/testutil.sh"

WORK=$(mktemp -d)
# shellcheck disable=SC2064
trap "rm -rf '$WORK'" EXIT

PLUGIN="$WORK/plugin"
DATA="$WORK/data"
LOG="$WORK/calls.log"
mkdir -p "$PLUGIN/bin" "$PLUGIN/hooks" "$PLUGIN/.claude-plugin" "$DATA"
cp "$HOOKS_SRC_DIR/../bin/legion" "$PLUGIN/bin/legion"

# fake_binary VERSION -- a legion binary that logs every call it gets.
fake_binary() {
  cat > "$DATA/legion" <<SH
#!/bin/bash
echo "bin-$1 \$*" >> "$LOG"
if [ "\${1:-}" = "--version" ]; then echo "legion $1 (build test)"; exit 0; fi
echo "ran $1: \$*"
SH
  chmod +x "$DATA/legion"
}

# The stub installer stands in for the download: it installs
# FAKE_NEW_VERSION, or installs nothing when that is unset (offline).
cat > "$PLUGIN/hooks/setup-binary.sh" <<SH
#!/bin/bash
echo "setup" >> "$LOG"
echo "setup stdout"
[ -n "\${FAKE_NEW_VERSION:-}" ] || exit 1
$(declare -f fake_binary)
LOG="$LOG" DATA="$DATA" fake_binary "\$FAKE_NEW_VERSION"
SH

plugin_version() {
  printf '{\n  "name": "legion",\n  "version": "%s"\n}\n' "$1" > "$PLUGIN/.claude-plugin/plugin.json"
}

reset() {
  rm -rf "$DATA" "$LOG"
  mkdir -p "$DATA"
  : > "$LOG"
  unset FAKE_NEW_VERSION
}

run() {
  CLAUDE_PLUGIN_DATA="$DATA" bash "$PLUGIN/bin/legion" "$@" 2> "$WORK/stderr"
}

count() { grep -c -- "$1" "$LOG" || true; }

echo "==> a current binary runs with no setup and primes the stamp"
reset; plugin_version 0.44.1; fake_binary 0.44.1
out=$(run recall --repo x)
assert_rc "call succeeds" 0 "$?"
assert_eq "stdout is the command's output" "$out" "ran 0.44.1: recall --repo x"
assert_eq "no setup call" "$(count setup)" "0"
assert_eq "stamp holds the verified version" "$(cat "$DATA/.version")" "0.44.1"

echo "==> a valid stamp skips the --version probe"
: > "$LOG"
run status >/dev/null
assert_eq "no --version probe" "$(count --version)" "0"

echo "==> an older binary is updated once, the daemon restarted, stdout clean"
reset; plugin_version 0.44.1; fake_binary 0.43.5
export FAKE_NEW_VERSION=0.44.1
out=$(run recall --repo x)
assert_rc "call succeeds" 0 "$?"
assert_eq "stdout is only the new binary's output" "$out" "ran 0.44.1: recall --repo x"
assert_eq "setup ran once" "$(count setup)" "1"
assert_eq "daemon restarted by the new binary" "$(count "bin-0.44.1 daemon-restart")" "1"
assert_eq "stamp updated" "$(cat "$DATA/.version")" "0.44.1"
assert_contains "stderr names the update" "$(cat "$WORK/stderr")" "older than plugin 0.44.1"
assert_eq "lock released" "$([ -e "$DATA/.setup.lock" ] && echo held || echo free)" "free"
: > "$LOG"
run status >/dev/null
assert_eq "the next call does not update again" "$(count setup)" "0"

echo "==> LEGION_NO_DAEMON=1 updates without a daemon restart"
reset; plugin_version 0.44.1; fake_binary 0.43.5
export FAKE_NEW_VERSION=0.44.1
LEGION_NO_DAEMON=1 run status >/dev/null
assert_eq "setup ran" "$(count setup)" "1"
assert_eq "no daemon restart" "$(count daemon-restart)" "0"

echo "==> a failed update warns and runs the old binary"
reset; plugin_version 0.44.1; fake_binary 0.43.5
out=$(run recall --repo x)
assert_rc "call still succeeds" 0 "$?"
assert_eq "old binary ran" "$out" "ran 0.43.5: recall --repo x"
assert_contains "stderr warns" "$(cat "$WORK/stderr")" "update to 0.44.1 failed"
assert_eq "no daemon restart" "$(count daemon-restart)" "0"
assert_eq "stamp keeps the old version" "$(cat "$DATA/.version")" "0.43.5"

echo "==> after a failure, the next try waits ten minutes"
: > "$LOG"
export FAKE_NEW_VERSION=0.44.1
run status >/dev/null
assert_eq "no retry inside the backoff" "$(count setup)" "0"
touch -t 202001010000 "$DATA/.setup.failed"
out=$(run status)
assert_eq "retry after the backoff" "$(count setup)" "1"
assert_eq "new binary ran" "$out" "ran 0.44.1: status"
assert_eq "failure marker cleared" "$([ -e "$DATA/.setup.failed" ] && echo set || echo clear)" "clear"

echo "==> an older plugin copy never pulls the binary down"
reset; plugin_version 0.43.5; fake_binary 0.44.1
export FAKE_NEW_VERSION=0.43.5
out=$(run status)
assert_eq "no setup call" "$(count setup)" "0"
assert_eq "newer binary ran" "$out" "ran 0.44.1: status"

echo "==> versions compare numerically, not as text"
reset; plugin_version 0.44.10; fake_binary 0.44.9
export FAKE_NEW_VERSION=0.44.10
run status >/dev/null
assert_eq "0.44.9 < 0.44.10 triggers setup" "$(count setup)" "1"

echo "==> a held lock skips the update"
reset; plugin_version 0.44.1; fake_binary 0.43.5
export FAKE_NEW_VERSION=0.44.1
mkdir "$DATA/.setup.lock"
out=$(run status)
assert_eq "no setup call" "$(count setup)" "0"
assert_eq "old binary ran" "$out" "ran 0.43.5: status"
assert_eq "another caller's lock is left alone" "$([ -d "$DATA/.setup.lock" ] && echo held || echo free)" "held"

echo "==> a stale lock is cleared"
touch -t 202001010000 "$DATA/.setup.lock"
run status >/dev/null
assert_eq "setup ran" "$(count setup)" "1"

echo "==> LEGION_SELF_HEAL=0 skips the check"
reset; plugin_version 0.44.1; fake_binary 0.43.5
export FAKE_NEW_VERSION=0.44.1
out=$(LEGION_SELF_HEAL=0 run status)
assert_eq "no setup call" "$(count setup)" "0"
assert_eq "binary ran as is" "$out" "ran 0.43.5: status"

echo "==> a binary replaced after the stamp is probed again"
reset; plugin_version 0.44.1; fake_binary 0.44.1
run status >/dev/null
printf '0.43.5\n' > "$DATA/.version"
touch -t 202001010000 "$DATA/.version"
: > "$LOG"
run status >/dev/null
assert_eq "--version probed" "$(count --version)" "1"
assert_eq "no setup call" "$(count setup)" "0"
assert_eq "stamp corrected" "$(cat "$DATA/.version")" "0.44.1"

finish_tests
