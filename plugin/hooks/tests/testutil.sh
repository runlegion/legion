#!/bin/bash
# Shared test utilities for the plugin hook harnesses (#614).
#
# Before this file, every test-*.sh redefined its own assert family and 8
# of them built bespoke stub-legion heredocs, each independently pinning a
# slice of the real CLI's output formats. The stub-legion contract is now
# defined ONCE, here: a real CLI output-format change is a one-file test
# update instead of an 8-file hunt with silent-staleness failure modes.
#
# Usage from a test-*.sh (they live one directory up):
#
#   # shellcheck source=tests/testutil.sh
#   source "$(dirname "${BASH_SOURCE[0]}")/tests/testutil.sh"
#
#   make_plugin_root my-hook.sh        # fake CLAUDE_PLUGIN_ROOT + stub legion
#   out=$(echo "$EVENT_JSON" | bash "$CLAUDE_PLUGIN_ROOT/hooks/my-hook.sh")
#   assert_contains "desc" "$out" "needle"
#   finish_tests
#
# make_plugin_root traps EXIT to remove its temp tree. A test that creates
# extra artifacts outside $WORK must install its own combined trap AFTER
# calling make_plugin_root (trap replaces, it does not stack).
#
# Stub-legion contract: responses are selected per call via FAKE_* env
# vars (see make_stub_legion below), so one stub serves every harness.

# Directory holding the production hooks (this file lives in hooks/tests/).
HOOKS_SRC_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

PASS=0
FAIL=0

# ---------- assert family ----------

assert_eq() {
  local desc="$1" actual="$2" expected="$3"
  if [ "$actual" = "$expected" ]; then
    PASS=$((PASS + 1))
    echo "  PASS: $desc"
  else
    FAIL=$((FAIL + 1))
    echo "  FAIL: $desc (expected '$expected', got '$actual')" >&2
  fi
}

assert_contains() {
  local desc="$1" haystack="$2" needle="$3"
  if echo "$haystack" | grep -q -- "$needle"; then
    PASS=$((PASS + 1))
    echo "  PASS: $desc"
  else
    FAIL=$((FAIL + 1))
    echo "  FAIL: $desc" >&2
    echo "    expected to find: $needle" >&2
    echo "    in: $haystack" >&2
  fi
}

assert_not_contains() {
  local desc="$1" haystack="$2" needle="$3"
  if echo "$haystack" | grep -q -- "$needle"; then
    FAIL=$((FAIL + 1))
    echo "  FAIL: $desc" >&2
    echo "    expected NOT to find: $needle" >&2
    echo "    in: $haystack" >&2
  else
    PASS=$((PASS + 1))
    echo "  PASS: $desc"
  fi
}

assert_empty() {
  local desc="$1" actual="$2"
  if [ -z "$actual" ]; then
    PASS=$((PASS + 1))
    echo "  PASS: $desc"
  else
    FAIL=$((FAIL + 1))
    echo "  FAIL: $desc" >&2
    echo "    expected empty, got: $actual" >&2
  fi
}

# assert_rc DESC EXPECTED_RC ACTUAL_RC
assert_rc() {
  local desc="$1" expected="$2" actual="$3"
  if [ "$actual" -eq "$expected" ]; then
    PASS=$((PASS + 1))
    echo "  PASS: $desc"
  else
    FAIL=$((FAIL + 1))
    echo "  FAIL: $desc (expected rc=$expected, got rc=$actual)" >&2
  fi
}

assert_file_contains() {
  local desc="$1" file="$2" needle="$3"
  if [ -f "$file" ] && grep -q -- "$needle" "$file"; then
    PASS=$((PASS + 1))
    echo "  PASS: $desc"
  else
    FAIL=$((FAIL + 1))
    echo "  FAIL: $desc" >&2
    echo "    expected $file to contain: $needle" >&2
    [ -f "$file" ] && echo "    actual: $(cat "$file")" >&2
  fi
}

assert_file_not_contains() {
  local desc="$1" file="$2" needle="$3"
  if [ ! -f "$file" ] || ! grep -q -- "$needle" "$file" 2>/dev/null; then
    PASS=$((PASS + 1))
    echo "  PASS: $desc"
  else
    FAIL=$((FAIL + 1))
    echo "  FAIL: $desc (unexpectedly found: $needle)" >&2
    echo "    in file: $file" >&2
  fi
}

assert_file_absent() {
  local desc="$1" file="$2"
  if [ ! -f "$file" ]; then
    PASS=$((PASS + 1))
    echo "  PASS: $desc"
  else
    FAIL=$((FAIL + 1))
    echo "  FAIL: $desc" >&2
    echo "    expected $file to be absent" >&2
  fi
}

# finish_tests -- print the summary and exit 1 on any failed assertion.
finish_tests() {
  echo
  echo "==> $PASS passed, $FAIL failed"
  if [ "$FAIL" -gt 0 ]; then
    exit 1
  fi
  exit 0
}

# ---------- fixtures ----------

# make_stub_legion PATH -- write the parameterized stub legion binary.
# Responses are selected at CALL time by FAKE_* env vars, so one stub
# serves every harness and a CLI output-format change is edited here only:
#
#   FAKE_BROKEN=1            every invocation exits 1 immediately
#   LEGION_STUB_LOG=<file>   append every invocation's argv (all commands)
#   FAKE_VERSION             `--version` -> "legion $FAKE_VERSION" (9.9.9)
#   FAKE_BUILD               when set, `--version` appends " (build $FAKE_BUILD)"
#                            (the #698 build-id suffix); unset -> no suffix
#   FAKE_WATCH               `watch list` body ("repo<TAB>/path" lines)
#   FAKE_STATS="repo:N"      `stats --repo repo` -> "repo: N reflections (...)"
#                            (anything else -> "no reflections stored yet")
#   FAKE_INDEX_JSON          `index --status --json` body (default [])
#   FAKE_SYM_LOCAL           space-separated symbols `sym def --json` answers
#                            with one hit in FAKE_SYM_LOCAL_REPO (legion)
#   FAKE_SYM_REMOTE          symbols answered with one hit in
#                            FAKE_SYM_REMOTE_REPO (huttspawn); others -> []
#   FAKE_SYM_REFS_JSON       `sym refs --json` body (default [])
#   FAKE_RECALL              `recall` body (default empty)
#   FAKE_KANBAN_ACCEPTED     `kanban list` -> one accepted card titled this
#   FAKE_KANBAN_DELEGATED_DEAD  `kanban delegated-needs-attention` -> one
#                            not-live delegated card titled this (#778)
#   FAKE_GOAL                `goal` body
#   FAKE_WHOAMI_BODY         `whoami` body below the standard banner header
#   FAKE_PREDICTION_ID       `uncertainty emit` row id (pred-fixed-1)
#   FAKE_WITNESS_LOG=<file>  `uncertainty witness` appends its argv here
#   FAKE_SPAWN_LOG=<file>    `serve` appends "spawned at <epoch>" here;
#                            `daemon-restart` appends "daemon-restart at <epoch>"
#   LEGION_TEST_MARKER=<file> `telemetry ...` appends its argv (sans
#                            leading "telemetry") here
make_stub_legion() {
  local path="$1"
  cat > "$path" <<'EOF'
#!/bin/bash
# Parameterized stub legion (tests/testutil.sh contract). FAKE_* env vars
# select responses; see make_stub_legion docs.
if [ "${FAKE_BROKEN:-}" = "1" ]; then
  exit 1
fi
if [ -n "${LEGION_STUB_LOG:-}" ]; then
  echo "$@" >> "$LEGION_STUB_LOG"
fi
case "${1:-}" in
  --version)
    # FAKE_BUILD (optional) appends the "(build <id>)" suffix the real CLI
    # carries since #698. Unset -> bare "legion <version>" (pre-#698 shape),
    # so harnesses that do not care about build id are unaffected.
    if [ -n "${FAKE_BUILD:-}" ]; then
      echo "legion ${FAKE_VERSION:-9.9.9} (build ${FAKE_BUILD})"
    else
      echo "legion ${FAKE_VERSION:-9.9.9}"
    fi
    ;;
  watch)
    if [ "${2:-}" = "list" ]; then
      printf '%s\n' "${FAKE_WATCH:-}"
    fi
    ;;
  stats)
    # Args: stats --repo NAME. Mirrors the real CLI stats line that
    # _legion-covered.sh regex-matches.
    repo="${3:-}"
    case "${FAKE_STATS:-}" in
      "$repo:"*)
        n="${FAKE_STATS#*:}"
        printf '%s: %s reflections (2026-01-01 to 2026-01-01)\n' "$repo" "$n"
        ;;
      *)
        printf 'no reflections stored yet\n'
        ;;
    esac
    ;;
  index)
    if [ "${2:-}" = "--status" ] && [ "${3:-}" = "--json" ]; then
      printf '%s\n' "${FAKE_INDEX_JSON:-[]}"
    fi
    ;;
  sym)
    if [ "${2:-}" = "def" ] && [ "${3:-}" = "--json" ]; then
      sym="${4:-}"
      case " ${FAKE_SYM_LOCAL:-} " in
        *" $sym "*)
          printf '[{"file":"src/main.rs","line":42,"symbol":"%s","repo":"%s","lang":"rust"}]\n' \
            "$sym" "${FAKE_SYM_LOCAL_REPO:-legion}"
          exit 0
          ;;
      esac
      case " ${FAKE_SYM_REMOTE:-} " in
        *" $sym "*)
          printf '[{"file":"src/foo.ts","line":10,"symbol":"%s","repo":"%s","lang":"typescript"}]\n' \
            "$sym" "${FAKE_SYM_REMOTE_REPO:-huttspawn}"
          exit 0
          ;;
      esac
      echo '[]'
    elif [ "${2:-}" = "refs" ] && [ "${3:-}" = "--json" ]; then
      printf '%s\n' "${FAKE_SYM_REFS_JSON:-[]}"
    fi
    ;;
  recall)
    printf '%s\n' "${FAKE_RECALL:-}"
    ;;
  kanban)
    if [ "${2:-}" = "list" ] && [ -n "${FAKE_KANBAN_ACCEPTED:-}" ]; then
      printf '{"status":"accepted","id":"42","title":"%s"}\n' "$FAKE_KANBAN_ACCEPTED"
    elif [ "${2:-}" = "delegated-needs-attention" ] && [ -n "${FAKE_KANBAN_DELEGATED_DEAD:-}" ]; then
      printf '{"status":"delegated","id":"43","title":"%s"}\n' "$FAKE_KANBAN_DELEGATED_DEAD"
    fi
    ;;
  goal)
    [ -n "${FAKE_GOAL:-}" ] && printf '%s\n' "$FAKE_GOAL"
    ;;
  whoami)
    echo "=== WHO YOU ARE -- READ THIS ==="
    echo "[Legion] Identity for test:"
    if [ -n "${FAKE_WHOAMI_BODY:-}" ]; then
      printf '%s\n' "$FAKE_WHOAMI_BODY"
    fi
    ;;
  uncertainty)
    case "${2:-}" in
      emit)
        printf '{"id":"%s","orphan_after":"2026-06-01T00:00:00Z"}\n' \
          "${FAKE_PREDICTION_ID:-pred-fixed-1}"
        ;;
      witness)
        echo "$@" >> "${FAKE_WITNESS_LOG:-/dev/null}"
        ;;
    esac
    ;;
  serve)
    echo "spawned at $(date +%s)" >> "${FAKE_SPAWN_LOG:-/dev/null}"
    ;;
  daemon-restart)
    echo "daemon-restart at $(date +%s)" >> "${FAKE_SPAWN_LOG:-/dev/null}"
    ;;
  telemetry)
    shift
    echo "$@" >> "${LEGION_TEST_MARKER:-/dev/null}"
    ;;
esac
exit 0
EOF
  chmod +x "$path"
}

# make_plugin_root [HOOK...] -- build a fake CLAUDE_PLUGIN_ROOT in a temp
# tree: lib/ + the shared helper files + the named hooks + the stub legion
# binary. Exports CLAUDE_PLUGIN_ROOT, XDG_CACHE_HOME, XDG_STATE_HOME and
# traps EXIT to clean the tree. Also sources lib/prelude.sh into the test
# shell so helpers like legion_hash_str are available to assertions.
make_plugin_root() {
  WORK=$(mktemp -d)
  # Expand $WORK now: the trap target is fixed at creation time.
  # shellcheck disable=SC2064
  trap "rm -rf '$WORK'" EXIT
  mkdir -p "$WORK/plugin/bin" "$WORK/plugin/hooks/lib" "$WORK/cache" "$WORK/state/legion"
  cp "$HOOKS_SRC_DIR/lib/prelude.sh" "$HOOKS_SRC_DIR/lib/emit.sh" "$WORK/plugin/hooks/lib/"
  cp "$HOOKS_SRC_DIR/_legion-covered.sh" \
     "$HOOKS_SRC_DIR/_legion-indexed.sh" \
     "$HOOKS_SRC_DIR/_legion-prequery.sh" \
     "$WORK/plugin/hooks/"
  local hook
  for hook in "$@"; do
    cp "$HOOKS_SRC_DIR/$hook" "$WORK/plugin/hooks/"
  done
  make_stub_legion "$WORK/plugin/bin/legion"
  export CLAUDE_PLUGIN_ROOT="$WORK/plugin"
  export XDG_CACHE_HOME="$WORK/cache"
  export XDG_STATE_HOME="$WORK/state"
  # shellcheck source=../lib/prelude.sh
  source "$WORK/plugin/hooks/lib/prelude.sh"
}
