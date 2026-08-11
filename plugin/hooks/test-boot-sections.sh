#!/bin/bash
# Test runner for lib/boot-sections.sh and its two callers (#879).
#
# #879: session-start.sh (cold boot) and post-compact.sh (after
# compaction) each inlined their own SessionStart banner section list, and
# the lists had silently diverged -- compact emitted only a checkpoint
# block, dropping identity, operating contract, pending replies, index
# status, kanban, goal, and autonomy budget with no error. The fix makes
# the parity STRUCTURAL: both hooks now source lib/boot-sections.sh and
# call the single emit_boot_core() driver, which takes no arguments and
# branches on nothing.
#
# Two tiers:
#   Tier 1 -- structural lock (static text checks, no subprocess exec):
#     both hooks source boot-sections.sh and call emit_boot_core; neither
#     hook's own source re-inlines a core "$LEGION" call outside
#     boot-sections.sh; every boot_section_<name> defined in
#     boot-sections.sh is registered in LEGION_BOOT_SECTIONS and vice
#     versa; emit_boot_core's own body takes no arguments ($1/$@/$# absent).
#   Tier 2 -- behavioral parity (the fail-before/pass-after proof): nine
#     sentinel FAKE_* values run through both real hook scripts via
#     make_plugin_root/make_stub_legion; every sentinel must appear in
#     BOTH additionalContext outputs. Each sentinel is asserted on EACH
#     output independently (never as an equality between the two, and
#     never one-sided) -- an equality-shaped or single-output assert would
#     pass vacuously if a stub case were miswired and both sides came back
#     empty.
#
# Tier 2's sentinel list is hand-maintained (one FAKE_* per section
# currently in LEGION_BOOT_SECTIONS). Tier 1's set-equality check
# generalizes to a section added later, but a section added to
# LEGION_BOOT_SECTIONS with no matching stub case in tests/testutil.sh
# passes Tier 1 and silently renders empty in Tier 2 rather than failing
# loudly -- add the FAKE_* case and the sentinel here when you add a
# section.
#
# Run from anywhere:
#   bash plugin/hooks/test-boot-sections.sh

set -u

# shellcheck source=tests/testutil.sh
source "$(dirname "${BASH_SOURCE[0]}")/tests/testutil.sh"

SESSION_START_SRC="$HOOKS_SRC_DIR/session-start.sh"
POST_COMPACT_SRC="$HOOKS_SRC_DIR/post-compact.sh"
BOOT_SECTIONS_SRC="$HOOKS_SRC_DIR/lib/boot-sections.sh"

# =========================== Tier 1: structural ===========================

echo "==> both hooks source lib/boot-sections.sh"
assert_contains "session-start.sh sources boot-sections.sh" "$(cat "$SESSION_START_SRC")" 'hooks/lib/boot-sections.sh'
assert_contains "post-compact.sh sources boot-sections.sh" "$(cat "$POST_COMPACT_SRC")" 'hooks/lib/boot-sections.sh'

echo "==> both hooks call emit_boot_core"
# The needle is the CALL shape, '$(emit_boot_core)', not the bare word --
# the bare word also matches inside a comment (e.g. a header prose
# reference to "the emit_boot_core driver"), which would let this check
# pass while the actual invocation was deleted.
# shellcheck disable=SC2016  # single-quoted on purpose: literal grep needle, not expansion
CALL_NEEDLE='$(emit_boot_core)'
assert_contains "session-start.sh calls emit_boot_core" "$(cat "$SESSION_START_SRC")" "$CALL_NEEDLE"
assert_contains "post-compact.sh calls emit_boot_core" "$(cat "$POST_COMPACT_SRC")" "$CALL_NEEDLE"

echo "==> neither hook re-inlines a core banner call outside boot-sections.sh"
# The closed set of literal calls that may ONLY appear inside
# lib/boot-sections.sh. A future maintainer re-inlining one of these
# directly into a hook (instead of registering a boot_section_<name> once)
# is exactly the per-path divergence #879 fixed.
# REGEX, not literal substrings. The first version of this list used literal
# needles like '"$LEGION" whoami', which miss every other spelling of the same
# call -- "${LEGION}" whoami, $LEGION whoami, '"'"'${LEGION}'"'"' whoami -- and this
# repo's own house style is the brace form (see "${CLAUDE_PLUGIN_ROOT:-}"
# above). A guard that only catches the spelling its author happened to think
# of is the same defect the Tier-1 positional-parameter lock had; both are
# fixed the same way and for the same reason.
# shellcheck disable=SC2016  # single-quoted on purpose: regex, not expansion
FORBIDDEN_PATTERNS=(
  '[$]\{?LEGION\}?"? +whoami'
  '[$]\{?LEGION\}?"? +whatami'
  '[$]\{?LEGION\}?"? +pending-replies'
  '[$]\{?LEGION\}?"? +kanban'
  '[$]\{?LEGION\}?"? +goal'
  '[$]\{?LEGION\}?"? +autonomy +status'
  '[$]\{?LEGION\}?"? +index'
  '[$]\{?LEGION\}?"? +now +--banner'
  '--domain +checkpoint'
)
for pattern in "${FORBIDDEN_PATTERNS[@]}"; do
  for src in "$SESSION_START_SRC" "$POST_COMPACT_SRC"; do
    # `--` is load-bearing: the `--domain checkpoint` pattern starts with a
    # dash and grep parses it as a flag without it, yielding empty output
    # and a green-looking assertion for a guard that never ran.
    hit=$(grep -cE -- "$pattern" "$src" || true)
    assert_eq "$(basename "$src") does not re-inline: ${pattern}" "0" "$hit"
  done
done

echo "==> every boot_section_<name>() defined is registered in LEGION_BOOT_SECTIONS, and vice versa"
DEFINED_SECTIONS=$(grep -oE '^boot_section_[a-zA-Z0-9_]+\(\)' "$BOOT_SECTIONS_SRC" \
  | sed -E 's/^boot_section_//; s/\(\)$//' | sort)
ARRAY_LINE=$(grep -E '^LEGION_BOOT_SECTIONS=\(' "$BOOT_SECTIONS_SRC")
REGISTERED_SECTIONS=$(printf '%s\n' "$ARRAY_LINE" \
  | sed -E 's/^LEGION_BOOT_SECTIONS=\(//; s/\)$//' | tr ' ' '\n' | sort)
assert_eq "defined boot_section_* set equals LEGION_BOOT_SECTIONS array" \
  "$DEFINED_SECTIONS" "$REGISTERED_SECTIONS"

echo "==> emit_boot_core takes no arguments (no positional parameter in its body)"
EMIT_BODY=$(sed -n '/^emit_boot_core() {/,/^}/p' "$BOOT_SECTIONS_SRC")
# Matches BOTH bare and brace forms: $1 $@ $# $* and ${1} ${1:-} ${@} ${#}.
# A literal three-needle check for '$1'/'$@'/'$#' misses every brace form --
# "${1:-}" does not contain the substring "$1" -- so `if [ -n "${1:-}" ]`
# smuggled in here passed the old lock silently. Measured: injecting exactly
# that turned the suite 45/0 GREEN before this change, and red after.
# Array expansions like "${arr[@]}" are not matched: the char after `${` is
# a letter, not one of [0-9@#*].
POSITIONAL_USE=$(printf '%s\n' "$EMIT_BODY" | grep -oE '[$]\{?[0-9@#*]' | sort -u | tr '\n' ' ')
assert_eq "no positional parameter (bare or brace form) in emit_boot_core" \
  "" "$POSITIONAL_USE"

# =========================== Tier 2: behavioral ============================

echo "==> behavioral parity: every core sentinel reaches BOTH hooks' additionalContext"

make_plugin_root session-start.sh post-compact.sh

# A plain scratch dir, not a git repo -- post-compact's git calls are all
# 2>/dev/null and emit_context runs unconditionally regardless, so no git
# state is required to prove the CORE sentinels ride through. LEGION_REPO
# is set explicitly rather than relying on basename($CWD).
SCRATCH_CWD="$WORK/scratch-cwd"
mkdir -p "$SCRATCH_CWD"

export LEGION_REPO="boot-sections-test"
export LEGION_NO_SYNC="1"

export FAKE_NOW_BANNER="SENTINEL_NOW_ABC"
export FAKE_WHOAMI_BODY="SENTINEL_IDENTITY_ABC"
export FAKE_WHATAMI_BODY="SENTINEL_WHATAMI_ABC"
export FAKE_PENDING_REPLIES="SENTINEL_PENDING_ABC"
export FAKE_CHECKPOINT="SENTINEL_CHECKPOINT_ABC"
export FAKE_INDEX_BANNER="SENTINEL_INDEX_ABC"
export FAKE_KANBAN_ACCEPTED="SENTINEL_KANBAN_ABC"
export FAKE_GOAL="SENTINEL_GOAL_ABC"
export FAKE_AUTONOMY_BANNER="SENTINEL_AUTONOMY_ABC"

SENTINELS=(
  "SENTINEL_NOW_ABC"
  "SENTINEL_IDENTITY_ABC"
  "SENTINEL_WHATAMI_ABC"
  "SENTINEL_PENDING_ABC"
  "SENTINEL_CHECKPOINT_ABC"
  "SENTINEL_INDEX_ABC"
  "SENTINEL_KANBAN_ABC"
  "SENTINEL_GOAL_ABC"
  "SENTINEL_AUTONOMY_ABC"
)

# SENTINELS above is in LEGION_BOOT_SECTIONS order on purpose: this block
# asserts the rendered SEQUENCE, not just presence. Presence assertions alone
# stay green if someone swaps `pending` and `identity` in the array -- which is
# precisely the #338 regression the comment beside that array memorializes
# (pending-replies ahead of identity buried the banner and the agent fell back
# to generic Claude prose). Criterion 1 of #879 is an ORDER claim, so presence
# was the wrong kind of evidence for it.
assert_sentinel_order() {
  local label="$1" haystack="$2"
  local prev=-1 idx rest sentinel bad=""
  for sentinel in "${SENTINELS[@]}"; do
    case "$haystack" in
      *"$sentinel"*) rest="${haystack#*"$sentinel"}"; idx=$(( ${#haystack} - ${#rest} )) ;;
      *) bad="${bad} ${sentinel}(absent)"; continue ;;
    esac
    if [ "$idx" -le "$prev" ]; then
      bad="${bad} ${sentinel}(out-of-order)"
    fi
    prev="$idx"
  done
  assert_eq "$label emits sections in LEGION_BOOT_SECTIONS order" "" "$bad"
}

EVENT_JSON="{\"cwd\":\"${SCRATCH_CWD}\",\"session_id\":\"boot-sections-test\"}"

SESSION_START_OUT=$(printf '%s' "$EVENT_JSON" | bash "$CLAUDE_PLUGIN_ROOT/hooks/session-start.sh")
POST_COMPACT_OUT=$(printf '%s' "$EVENT_JSON" | bash "$CLAUDE_PLUGIN_ROOT/hooks/post-compact.sh")

SESSION_START_CTX=$(printf '%s' "$SESSION_START_OUT" | jq -r '.hookSpecificOutput.additionalContext // empty')
POST_COMPACT_CTX=$(printf '%s' "$POST_COMPACT_OUT" | jq -r '.hookSpecificOutput.additionalContext // empty')

for sentinel in "${SENTINELS[@]}"; do
  # Each output asserted independently on purpose -- see file header.
  assert_contains "session-start.sh additionalContext contains ${sentinel}" "$SESSION_START_CTX" "$sentinel"
  assert_contains "post-compact.sh additionalContext contains ${sentinel}" "$POST_COMPACT_CTX" "$sentinel"
done

echo "==> checkpoint section falls back to domain=snooze on BOTH hooks when no checkpoint exists"
# legion_boot_fetch_checkpoint's snooze fallback lives once, in the shared
# lib, so this is not a per-hook behavior to re-verify per hook -- but the
# design explicitly claims post-compact "gains the snooze fallback for
# free" (it used to call plain `recall --domain checkpoint` with no
# fallback), so prove that claim on the hook that changed rather than
# asserting it only by reading the source.
unset FAKE_CHECKPOINT
export FAKE_SNOOZE="SENTINEL_SNOOZE_ABC"
POST_COMPACT_SNOOZE_OUT=$(printf '%s' "$EVENT_JSON" | bash "$CLAUDE_PLUGIN_ROOT/hooks/post-compact.sh")
POST_COMPACT_SNOOZE_CTX=$(printf '%s' "$POST_COMPACT_SNOOZE_OUT" | jq -r '.hookSpecificOutput.additionalContext // empty')
assert_contains "post-compact.sh falls back to domain=snooze" "$POST_COMPACT_SNOOZE_CTX" "SENTINEL_SNOOZE_ABC"
unset FAKE_SNOOZE

echo "==> sections render in LEGION_BOOT_SECTIONS order on BOTH hooks (#338)"
assert_sentinel_order "session-start.sh" "$SESSION_START_OUT"
assert_sentinel_order "post-compact.sh" "$POST_COMPACT_OUT"

finish_tests
