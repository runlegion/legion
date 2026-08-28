#!/bin/bash
# Shared SessionStart banner assembly (#879). Before this file, cold-boot
# (session-start.sh) and post-compact (post-compact.sh) each inlined their
# own section list, and the lists had diverged: startup emitted identity,
# operating contract, pending replies, checkpoint, index status, kanban,
# goal, and autonomy budget; compact emitted only a checkpoint block. An
# agent that compacted mid-session silently lost its identity, its
# operating contract, its pending replies, its work source, and its
# autonomy budget -- with no error, because nothing was wrong, the two
# hooks had just never been forced to agree.
#
# This file is the fix: ONE ordered list of sections and ONE function that
# renders them, sourced by BOTH hooks. A section registered here reaches
# both SessionStart matchers by construction -- there is no per-hook list
# left to diverge.
#
# Usage from a hook script (after sourcing lib/prelude.sh and lib/emit.sh,
# and after LOG is set):
#
#   # shellcheck source=lib/boot-sections.sh
#   source "${CLAUDE_PLUGIN_ROOT:-}/hooks/lib/boot-sections.sh" 2>/dev/null || exit 0
#
#   OUTPUT=$(emit_boot_core)
#
# ---------------------------------------------------------------------------
# INVARIANT: emit_boot_core takes NO arguments and branches on nothing. Do
# not add a parameter to select a subset of sections, an env var read
# inside this file to change behavior per caller, or any other form of
# caller-identity branching -- that reintroduces exactly the per-path
# divergence #879 fixed, just moved one level down. If a hook needs
# content the other must not have, that content is THAT HOOK'S OWN, added
# before/after the `emit_boot_core` call in the hook script -- never inside
# this function. (test-boot-sections.sh's Tier-1 lock greps this function's
# body for `$1`/`$@`/`$#` and fails the build if any appear, but the lock
# cannot catch an `if` that branches on something else entirely -- e.g. an
# env var set by only one caller. That case is on code review, not on this
# test. See lib/prelude.sh's "THE BOUNDARY" comment for the same pattern:
# some invariants are enforced by a test, some only by a comment a reviewer
# has to actually read.)
# ---------------------------------------------------------------------------

# Double-source guard.
if [ -n "${LEGION_BOOT_SECTIONS_SOURCED:-}" ]; then
  return 0
fi
LEGION_BOOT_SECTIONS_SOURCED=1

# append_block ACC BLOCK -- join BLOCK onto ACC with a blank-line separator,
# skipping empty blocks. Pure function: takes both operands as arguments and
# prints the result, touching no global. (An earlier draft mutated a global
# named OUTPUT -- both session-start.sh and post-compact.sh already own a
# variable by that name, and a shared lib silently clobbering a caller's
# variable is its own bug waiting to happen.)
append_block() {
  local acc="$1" block="$2"
  if [ -z "$block" ]; then
    printf '%s' "$acc"
    return
  fi
  if [ -n "$acc" ]; then
    printf '%s\n\n%s' "$acc" "$block"
  else
    printf '%s' "$block"
  fi
}

# legion_boot_fetch_checkpoint -- last checkpoint reflection. The
# /checkpoint command and the precompact safety-net both write
# domain=checkpoint; freshest wins. Transitional fallback: before the
# snooze->checkpoint rename (#568), the deliberate session summary lived in
# domain=snooze. Surface a legacy snooze reflection only when no checkpoint
# exists yet, so the first session after upgrade does not lose its anchor.
# Remove once domain=snooze has aged out.
# --preview 2000, not 500. Cold boot capped at 500 and post-compact passed
# no --preview at all (full text). Consolidating on 500 would silently
# truncate the checkpoint on the ONE path that exists to restore it -- a
# just-compacted agent has nothing else. 2000 matches the whoami banner cap
# (#342) and is applied to BOTH callers, since a per-caller preview length
# is exactly the branching this file forbids.
legion_boot_fetch_checkpoint() {
  local out
  out=$("$LEGION" recall --repo "$REPO" --domain checkpoint --limit 1 --preview 2000 2>>"$LOG")
  if [ -z "$out" ]; then
    out=$("$LEGION" recall --repo "$REPO" --domain snooze --limit 1 --preview 2000 2>>"$LOG")
  fi
  printf '%s' "$out"
}

# ---------- sections, in no particular order here -- LEGION_BOOT_SECTIONS
# below is what fixes the order. Each returns its rendered block, or empty
# to be skipped. ----------

# Now -- weekday + local time + sunphase. One line, lands first so an agent
# reads "today is Sunday afternoon" before identity primes voice.
# claude-code's own systemPrompt ships `currentDate` but no weekday and no
# hour, so agents pattern-match on conversation density and start saying
# "tonight" or "wind down" when the operator has the rest of the workday
# ahead. See #410.
boot_section_now() {
  "$LEGION" now --banner 2>>"$LOG"
}

# Identity -- who am I. Banner-wrapped by the binary.
boot_section_identity() {
  "$LEGION" whoami --repo "$REPO" --limit 5 2>>"$LOG"
}

# Operating contract -- how I operate (domain: workflow). Lands right after
# identity so the agent reads WHO YOU ARE, then HOW YOU OPERATE.
# Banner-wrapped by the binary; silent when the repo has no workflow roots
# yet.
boot_section_whatami() {
  "$LEGION" whatami --repo "$REPO" --limit 5 2>>"$LOG"
}

# Pending request-shaped signals -- directed asks waiting on a reply.
# Strong "REQUIRES A REPLY" framing prevents the system-reminder wrapper
# from causing the agent to no-op (the platform smugglr-fence RFC review
# regression, #318).
boot_section_pending() {
  "$LEGION" pending-replies --repo "$REPO" 2>>"$LOG"
}

# Last checkpoint -- where was I.
boot_section_checkpoint() {
  local out
  out=$(legion_boot_fetch_checkpoint)
  if [ -z "$out" ]; then
    # main's post-compact.sh printed a literal "(no checkpoint found)" here.
    # append_block skips empty sections, so consolidating without this left
    # post-compact's ACTION REQUIRED footer instructing the agent to "check
    # the checkpoint reflection" with nothing above it and no signal that the
    # absence was real rather than a fetch failure -- the ambiguous-silence
    # failure mode. Emitted on both paths, since a per-caller placeholder is
    # the branching this file forbids.
    printf '%s' "[Legion] No checkpoint found (none stored, or the last session ended without one)."
    return 0
  fi
  printf '%s' "$out"
}

# Index status -- one line if every detected language has a fresh index,
# multi-line block if anything is stale or missing. Silent when the repo is
# not in watch.toml or no language is detected. Lets the agent see whether
# `legion sym` will succeed before they call it.
boot_section_index() {
  "$LEGION" index "$REPO" --status --banner 2>>"$LOG"
}

# Watch heartbeat (#997) -- silent when the watch loop is alive, one line
# (index-status style) when it is stale or not running at all. Before this,
# a dead or stale watch loop was invisible at boot -- an agent had no signal
# that wakes/nudges were not happening, unlike index staleness which
# boot_section_index already surfaces. Host-wide (no --repo: `legion watch
# status` is not scoped to a repo).
#
# #1019: this used to switch on `legion watch status`'s PROSE first line
# (`status:  alive` / `status:  stale  (last beat: ...)` / `status:
# absent`) with nothing coupling those literals to src/cli/watch.rs -- a
# reformat of the prose muted this section with zero test failure on
# either side (surfaced in #1017's review). It now reads `watch status
# --json`'s stable one-line form
# (`{"status":"alive|stale|absent","last_beat_age":"<text>"|null}`) via
# jq, the same status literals `render_status_json`'s doc comment and its
# `watch_status_tests` in src/cli/watch.rs pin byte-for-byte, and this
# file's own test-boot-sections.sh FAKE_WATCH_STATUS fixtures use.
#
# Two distinct failure modes, handled differently on purpose:
#   - the CALL fails (missing jq, missing/broken binary, non-zero exit,
#     unparseable JSON that jq can't extract a status from) -> silent,
#     same fail-open contract as every other section. This is a call-layer
#     problem, not a signal worth surfacing.
#   - the call SUCCEEDS but returns a status value outside the three known
#     ones -- e.g. a fourth status a future watch.rs adds, or a renamed
#     literal -- -> a banner naming the raw value, never silence. This is
#     exactly the drift #1019 exists to make visible.
boot_section_watch() {
  command -v jq >/dev/null 2>&1 || return 0
  local out status age
  out=$("$LEGION" watch status --json 2>>"$LOG") || return 0
  status=$(printf '%s' "$out" | jq -r '.status // empty' 2>/dev/null)
  case "$status" in
    alive)
      return 0
      ;;
    absent)
      printf '[Legion] Watch: not running -- run legion daemon-spawn'
      ;;
    stale)
      age=$(printf '%s' "$out" | jq -r '.last_beat_age // empty' 2>/dev/null)
      printf '[Legion] Watch: stale (last beat: %s) -- run legion daemon-restart' "$age"
      ;;
    *)
      # Unknown status value (possibly empty, if the call succeeded but
      # jq could not find .status at all) -- print it rather than stay
      # silent, per #1019.
      printf '[Legion] Watch: %s -- run legion watch status' "$status"
      ;;
  esac
}

# Work source -- what's on my plate (#931: sourced live from the repo's
# configured work-source issues, no local board/goal state survives -- see
# src/queue.rs's module doc comment for why picking never claims anything,
# which is also why there is no separate goal section any more: "goal" was
# the active Accepted card's criteria, and there is no local "accepted"
# state left to derive it from).
boot_section_work() {
  local work
  work=$("$LEGION" work --repo "$REPO" --peek 2>>"$LOG")
  if [ -n "$work" ]; then
    printf '[Legion] Current work:\n%s' "$work"
  fi
}

# Autonomy budget (#524) -- remind the agent it has sanctioned units to
# spend on self-directed work, so it acts on the board instead of waiting to
# be told.
boot_section_budget() {
  "$LEGION" autonomy status --repo "$REPO" --banner 2>>"$LOG"
}

# The canonical order (#338: "identity before work" -- pending-replies
# prepended in front of identity drowned the banner under 100KB of
# REQUIRES A REPLY framing on rafters' rich-identity startup, and the agent
# defaulted to generic Claude prose instead of reading its identity chain).
# This array is now the ONLY place section order is decided -- it used to
# be re-derived independently by each hook's comment numbering.
LEGION_BOOT_SECTIONS=(now identity whatami pending checkpoint index watch work budget)

# emit_boot_core -- render every section in LEGION_BOOT_SECTIONS, in order,
# skipping empty ones, and print the joined result. No arguments. No
# caller-identity branching. See the file header before touching this.
emit_boot_core() {
  local result="" section block
  for section in "${LEGION_BOOT_SECTIONS[@]}"; do
    block=$("boot_section_${section}")
    result=$(append_block "$result" "$block")
  done
  printf '%s' "$result"
}
