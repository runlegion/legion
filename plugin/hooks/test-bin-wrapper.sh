#!/bin/bash
# Test runner for the bin/legion self-heal: a mid-session plugin update
# leaves the binary behind, because setup-binary.sh runs only on
# SessionStart "startup" and /reload-plugins fires no hook.
#
# Covers, against a stub installer: a current binary runs with no setup call
# and primes the version stamp; a valid stamp skips the --version probe; an
# older binary is updated once, the daemon restarted, and the command's
# stdout kept clean; a failed update warns, runs the old binary and backs off
# ten minutes; an older plugin copy never pulls the binary down; versions
# compare numerically, and one that is not plain numbers runs as is; a held lock skips the update and a stale one is
# cleared; LEGION_NO_DAEMON and LEGION_SELF_HEAL=0 are honoured.
#
# Then against the real setup-binary.sh, with a fake curl serving a fixture
# release: the install replaces the binary by rename, leaves no staging
# file, and writes a stamp the wrapper trusts; a matching binary primes the
# stamp; the startup path, with no wrapper, writes the stamp itself; a
# checksum mismatch leaves the old binary in place.
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

echo "==> a version that is not plain dotted numbers runs the binary as is"
for v in 0.44.1-dev 0.45.0-rc1 garbage; do
  reset; plugin_version 0.44.1; fake_binary "$v"
  out=$(run status)
  assert_rc "binary $v: call succeeds" 0 "$?"
  assert_eq "binary $v: it ran" "$out" "ran $v: status"
  assert_eq "binary $v: no setup call" "$(count setup)" "0"
done
reset; plugin_version 0.44.1; fake_binary 0.44.1
run status >/dev/null
# shellcheck disable=SC2016
printf '%s\n' 'a[$(touch "$WORK/pwned")]' > "$DATA/.version"
out=$(run status)
assert_rc "garbled stamp: call succeeds" 0 "$?"
assert_eq "garbled stamp: the binary ran" "$out" "ran 0.44.1: status"
assert_eq "garbled stamp: nothing in it ran" "$([ -e "$WORK/pwned" ] && echo ran || echo inert)" "inert"

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

# ---------- the real setup-binary.sh ----------

REAL="$WORK/real"
FIXTURE="$WORK/fixture"
FAKEBIN="$WORK/fakebin"
mkdir -p "$REAL/bin" "$REAL/hooks/lib" "$REAL/.claude-plugin" "$FIXTURE" "$FAKEBIN" "$WORK/home"
cp "$HOOKS_SRC_DIR/../bin/legion" "$REAL/bin/legion"
cp "$HOOKS_SRC_DIR/setup-binary.sh" "$REAL/hooks/setup-binary.sh"
cp "$HOOKS_SRC_DIR/lib/deny-mirror.sh" "$REAL/hooks/lib/deny-mirror.sh"
printf '{\n  "name": "legion",\n  "version": "0.44.1"\n}\n' > "$REAL/.claude-plugin/plugin.json"

case "$(uname -s)" in Darwin) OS=macos ;; *) OS=linux ;; esac
case "$(uname -m)" in arm64|aarch64) ARCH=arm64 ;; *) ARCH=x64 ;; esac
ARTIFACT="legion-${OS}-${ARCH}.tar.gz"

# The release fixture: a tarball holding a 0.44.1 binary, and its checksum.
mkdir "$FIXTURE/pkg"
DATA="$FIXTURE/pkg" LOG="$LOG" fake_binary 0.44.1
tar czf "$FIXTURE/$ARTIFACT" -C "$FIXTURE/pkg" legion
sha() { sha256sum "$1" 2>/dev/null | awk '{print $1}' || shasum -a 256 "$1" | awk '{print $1}'; }
printf '%s  %s\n' "$(sha "$FIXTURE/$ARTIFACT")" "$ARTIFACT" > "$FIXTURE/checksums.txt"

# Fake curl: `curl -fsSL -o OUT URL` copies the fixture file URL names.
cat > "$FAKEBIN/curl" <<SH
#!/bin/bash
out=""; url=""
while [ \$# -gt 0 ]; do
  case "\$1" in -o) out="\$2"; shift 2 ;; -*) shift ;; *) url="\$1"; shift ;; esac
done
echo "curl \$url" >> "$LOG"
cp "$FIXTURE/\${url##*/}" "\$out"
SH
chmod +x "$FAKEBIN/curl"

# PATH holds only the fake curl and the system tools: setup-binary.sh skips
# the download when any `legion` on PATH already reports the version.
SAFE_PATH="$FAKEBIN:/usr/bin:/bin"

# GNU stat first (`stat -f` is filesystem status there); BSD falls through.
inode() { stat -c %i "$1" 2>/dev/null || stat -f %i "$1"; }
staged_count() { find "$DATA" -name 'legion.new*' | wc -l | tr -d ' '; }

run_real() {
  HOME="$WORK/home" PATH="$SAFE_PATH" CLAUDE_PLUGIN_DATA="$DATA" LEGION_NO_DAEMON=1 \
    bash "$REAL/bin/legion" "$@" 2> "$WORK/stderr"
}

echo "==> the real installer replaces the binary by rename and writes the stamp"
reset; fake_binary 0.43.5
old_inode=$(inode "$DATA/legion")
out=$(run_real recall --repo x)
assert_rc "call succeeds" 0 "$?"
assert_eq "the new binary ran" "$out" "ran 0.44.1: recall --repo x"
assert_eq "downloaded once" "$(count "curl .*$ARTIFACT")" "1"
assert_eq "stamp written" "$(cat "$DATA/.version")" "0.44.1"
assert_eq "no staging file left" "$(staged_count)" "0"
assert_eq "a new inode replaced the old one" \
  "$([ "$(inode "$DATA/legion")" != "$old_inode" ] && echo renamed || echo same)" "renamed"
assert_eq "the binary is not newer than the stamp" \
  "$([ "$DATA/legion" -nt "$DATA/.version" ] && echo newer || echo ok)" "ok"
: > "$LOG"
run_real status >/dev/null
assert_eq "the next call trusts the stamp: no --version probe" "$(count --version)" "0"

echo "==> a matching binary primes the stamp from the startup path"
rm -f "$DATA/.version"
HOME="$WORK/home" PATH="$SAFE_PATH" CLAUDE_PLUGIN_ROOT="$REAL" CLAUDE_PLUGIN_DATA="$DATA" \
  LEGION_NO_DAEMON=1 LEGION_SELF_HEAL=0 bash "$REAL/hooks/setup-binary.sh" 2>/dev/null
assert_eq "stamp primed" "$(cat "$DATA/.version")" "0.44.1"

echo "==> the startup-path install, with no wrapper, writes the stamp itself"
reset; fake_binary 0.43.5
HOME="$WORK/home" PATH="$SAFE_PATH" CLAUDE_PLUGIN_ROOT="$REAL" CLAUDE_PLUGIN_DATA="$DATA" \
  LEGION_NO_DAEMON=1 LEGION_SELF_HEAL=0 bash "$REAL/hooks/setup-binary.sh" 2>/dev/null
assert_eq "binary installed" "$("$DATA/legion" --version)" "legion 0.44.1 (build test)"
assert_eq "stamp written by setup-binary.sh" "$(cat "$DATA/.version")" "0.44.1"
assert_eq "no staging file left" "$(staged_count)" "0"

echo "==> a checksum mismatch leaves the old binary in place"
reset; fake_binary 0.43.5
cp "$FIXTURE/checksums.txt" "$WORK/checksums.good"
printf '%s  %s\n' "0000" "$ARTIFACT" > "$FIXTURE/checksums.txt"
out=$(run_real status)
assert_rc "call succeeds" 0 "$?"
assert_eq "old binary ran" "$out" "ran 0.43.5: status"
assert_contains "stderr names the mismatch" "$(cat "$WORK/stderr")" "checksum mismatch"
assert_eq "no staging file left" "$(staged_count)" "0"
cp "$WORK/checksums.good" "$FIXTURE/checksums.txt"

finish_tests
