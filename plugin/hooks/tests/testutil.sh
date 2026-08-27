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
#   FAKE_NO_PUSH_TAG=1       `push --help` omits --tag, simulating a binary
#                            that predates #915 (default: --tag advertised)
#   FAKE_STATS="repo:N"      `stats --repo repo` -> "repo: N reflections (...)"
#                            (anything else -> "no reflections stored yet")
#   FAKE_INDEX_JSON          `index --status --json` body (default [])
#   FAKE_SYM_LOCAL           space-separated symbols `sym def --json` answers
#                            with one hit in FAKE_SYM_LOCAL_REPO (legion)
#   FAKE_SYM_REMOTE          symbols answered with one hit in
#                            FAKE_SYM_REMOTE_REPO (huttspawn); others -> []
#   FAKE_SYM_REFS_JSON       `sym refs --json` body (default [])
#   FAKE_RECALL              `recall` body, default for any `recall` call
#                            that does not carry --domain checkpoint or
#                            --domain snooze (default empty)
#   FAKE_CHECKPOINT          `recall --domain checkpoint` body; falls back
#                            to FAKE_RECALL when unset. NOTE this is a
#                            harness convenience, NOT a model of the real
#                            fallback -- production falls back
#                            checkpoint-domain -> SNOOZE-domain
#                            (legion_boot_fetch_checkpoint). Use FAKE_SNOOZE
#                            to exercise that path; this default does not
#   FAKE_SNOOZE              `recall --domain snooze` body (default empty;
#                            no fallback -- this IS the fallback tier)
#   FAKE_WORK_PEEK           `work --repo R --peek` -> one line naming this
#                            (#931: `boot_section_work`'s "what's on my
#                            plate" banner section)
#   FAKE_DELEGATED_NEEDS_ATTENTION  `watch delegated-needs-attention --json`
#                            -> one not-live delegated work-item row naming
#                            this as its work_item_id (#778, card-free since
#                            #931)
#   FAKE_WHOAMI_BODY         `whoami` body below the standard banner header
#   FAKE_WHATAMI_BODY        `whatami` body below the standard banner header
#   FAKE_PENDING_REPLIES     `pending-replies` body (default empty)
#   FAKE_NOW_BANNER          `now --banner` body (default empty)
#   FAKE_INDEX_BANNER        `index <repo> --status --banner` body (default
#                            empty; distinct from FAKE_INDEX_JSON, which
#                            answers the unrelated `index --status --json`
#                            shape _legion-indexed.sh calls)
#   FAKE_AUTONOMY_BANNER     `autonomy status --banner` body (default empty)
#   FAKE_BULLPEN_COUNT       `bullpen --count` body (default empty)
#   FAKE_PREDICTION_ID       `uncertainty emit` row id (pred-fixed-1)
#   FAKE_WITNESS_LOG=<file>  `uncertainty witness` appends its argv here
#   FAKE_SPAWN_LOG=<file>    `serve` or `daemon-spawn` appends "spawned at
#                            <epoch>" here; `daemon-restart` appends
#                            "daemon-restart at <epoch>" (#997: the
#                            supervisor's cold-start path calls
#                            `daemon-spawn`, never `serve`, but the stub
#                            keeps the `serve` case for any other caller)
#   LEGION_TEST_MARKER=<file> `telemetry ...` appends its argv (sans
#                            leading "telemetry") here
#   FAKE_DELIVER_DRAIN      `deliver drain` body (default empty, #941)
#   FAKE_WATCH_STATUS        `watch status` body (default empty, simulating
#                            an unreachable/erroring binary -- #997's
#                            `boot_section_watch` treats that as silent)
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
  push)
    # `push --help` is probed by no-git-push.sh (#915) to decide whether this
    # binary can push tags -- the plugin's hooks and the binary they drive can
    # be different versions, so the hook must not rewrite to a flag the binary
    # lacks. Advertises --tag by default (the current CLI); set
    # FAKE_NO_PUSH_TAG=1 to simulate a binary that predates it.
    if [ "${2:-}" = "--help" ]; then
      echo "Push a branch to origin -- the sanctioned in-band push path (#791)."
      echo "      --branch <BRANCH>  Branch to push"
      if [ "${FAKE_NO_PUSH_TAG:-}" != "1" ]; then
        echo "      --tag <TAG>        Tag to push"
      fi
    fi
    ;;
  watch)
    if [ "${2:-}" = "list" ]; then
      printf '%s\n' "${FAKE_WATCH:-}"
    elif [ "${2:-}" = "delegated-needs-attention" ] && [ -n "${FAKE_DELEGATED_NEEDS_ATTENTION:-}" ]; then
      printf '{"work_item_id":"%s","attempt_id":"attempt-fixed-1","repo":"%s"}\n' \
        "$FAKE_DELEGATED_NEEDS_ATTENTION" "${LEGION_REPO:-test}"
    elif [ "${2:-}" = "status" ]; then
      # #997: boot_section_watch's data source. FAKE_WATCH_STATUS carries the
      # whole `legion watch status` body (default empty, matching a binary
      # that exits non-zero or an empty response).
      [ -n "${FAKE_WATCH_STATUS:-}" ] && printf '%s\n' "$FAKE_WATCH_STATUS"
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
    elif [ "${3:-}" = "--status" ] && [ "${4:-}" = "--banner" ]; then
      # Real call: index "$REPO" --status --banner -- repo is $2, so the
      # discriminating flags land at $3/$4, not $2/$3 like the --json shape
      # above.
      [ -n "${FAKE_INDEX_BANNER:-}" ] && printf '%s\n' "$FAKE_INDEX_BANNER"
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
    # Discriminate by scanning argv for --domain rather than pinning a
    # position: real calls are `recall --repo R --domain X ...` (domain at
    # $4) or `recall --repo R --context Q ...` (no domain at all), and a
    # position-pinned check silently answers the wrong FAKE_* var when a
    # flag shifts.
    domain=""
    take_next="0"
    for arg in "$@"; do
      if [ "$take_next" = "1" ]; then
        domain="$arg"
        break
      fi
      if [ "$arg" = "--domain" ]; then
        take_next="1"
      fi
    done
    case "$domain" in
      checkpoint)
        printf '%s\n' "${FAKE_CHECKPOINT:-${FAKE_RECALL:-}}"
        ;;
      snooze)
        printf '%s\n' "${FAKE_SNOOZE:-}"
        ;;
      *)
        printf '%s\n' "${FAKE_RECALL:-}"
        ;;
    esac
    ;;
  work)
    # Real call: work --repo R --peek -- scan argv for --peek rather than
    # pinning a position, matching the `recall` case's discrimination style.
    has_peek="0"
    for arg in "$@"; do
      [ "$arg" = "--peek" ] && has_peek="1"
    done
    if [ "$has_peek" = "1" ] && [ -n "${FAKE_WORK_PEEK:-}" ]; then
      printf '%s\n' "$FAKE_WORK_PEEK"
    fi
    ;;
  whoami)
    echo "=== WHO YOU ARE -- READ THIS ==="
    echo "[Legion] Identity for test:"
    if [ -n "${FAKE_WHOAMI_BODY:-}" ]; then
      printf '%s\n' "$FAKE_WHOAMI_BODY"
    fi
    ;;
  whatami)
    echo "=== HOW YOU OPERATE -- READ THIS ==="
    echo "[Legion] Operating contract for test:"
    if [ -n "${FAKE_WHATAMI_BODY:-}" ]; then
      printf '%s\n' "$FAKE_WHATAMI_BODY"
    fi
    ;;
  pending-replies)
    [ -n "${FAKE_PENDING_REPLIES:-}" ] && printf '%s\n' "$FAKE_PENDING_REPLIES"
    ;;
  now)
    if [ "${2:-}" = "--banner" ]; then
      [ -n "${FAKE_NOW_BANNER:-}" ] && printf '%s\n' "$FAKE_NOW_BANNER"
    fi
    ;;
  autonomy)
    if [ "${2:-}" = "status" ]; then
      [ -n "${FAKE_AUTONOMY_BANNER:-}" ] && printf '%s\n' "$FAKE_AUTONOMY_BANNER"
    fi
    ;;
  bullpen)
    if [ "${2:-}" = "--count" ]; then
      [ -n "${FAKE_BULLPEN_COUNT:-}" ] && printf '%s\n' "$FAKE_BULLPEN_COUNT"
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
  daemon-spawn)
    echo "spawned at $(date +%s)" >> "${FAKE_SPAWN_LOG:-/dev/null}"
    ;;
  daemon-restart)
    echo "daemon-restart at $(date +%s)" >> "${FAKE_SPAWN_LOG:-/dev/null}"
    ;;
  telemetry)
    shift
    echo "$@" >> "${LEGION_TEST_MARKER:-/dev/null}"
    ;;
  deliver)
    if [ "${2:-}" = "drain" ]; then
      [ -n "${FAKE_DELIVER_DRAIN:-}" ] && printf '%s\n' "$FAKE_DELIVER_DRAIN"
    fi
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
  cp "$HOOKS_SRC_DIR/lib/prelude.sh" "$HOOKS_SRC_DIR/lib/emit.sh" \
     "$HOOKS_SRC_DIR/lib/boot-sections.sh" "$WORK/plugin/hooks/lib/"
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
