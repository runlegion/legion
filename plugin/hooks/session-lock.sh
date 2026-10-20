#!/bin/bash
# Legion interactive-session lock (#583, #996).
#
# Writes the per-repo `.session` lock so `legion watch` can see that a live
# agent holds this repo and declines to spawn a duplicate into its working
# tree. Claiming the lock also fires watch's preemption path, which
# terminates a watch-spawned worker already sitting on the repo -- two
# agents must not share a repo.
#
# WHY THIS IS ITS OWN HOOK RATHER THAN A LINE IN session-start.sh (#996):
# the lock must be held whenever a session PROCESS is live, however that
# session began, but the boot banner should not be re-emitted into a
# transcript that already carries it. Those two want different matchers, so
# they get different scripts. session-start.sh stays on `startup`; this runs
# on every variant that produces a live session.
#
# The bug this fixes: the lock used to be written only under
# matcher:"startup", so a session started with `claude --resume` never wrote
# one. Watch could not see it, spawned an agent into its tree, and killing
# that agent only freed the lock for the next poll to spawn another. Observed
# 2026-08-21: two agents committing to one branch minutes apart.
#
# `compact` is deliberately absent: compaction does not change the session
# pid, so the lock is already held and has no TTL to refresh. `fork` is
# deliberately absent too -- a fork is a second live process on one repo, and
# this per-repo lock can only name one pid, so overwriting the parent's would
# be a silent wrong answer rather than a fix. That case needs its own design.
#
# Non-blocking and fail-open, like every other hook on this path: it never
# adds latency to session start and never blocks the session on lock trouble.

# shellcheck source=lib/prelude.sh
source "${CLAUDE_PLUGIN_ROOT:-}/hooks/lib/prelude.sh" 2>/dev/null || exit 0

legion_hook_parse || exit 0

if [ -z "${REPO:-}" ]; then
  exit 0
fi

# legion_session_pid -- print the pid the lock must name: the long-lived
# session process, never this short-lived hook. The hook's parent is not
# always that process. Claude Code runs hooks as `/bin/sh -c "bash .../hooks/
# session-lock.sh"`; where that shell execs its last command (macOS) $PPID is
# the session, but where it does not (observed on Linux, #1400) $PPID is a
# shell wrapper that exits with the hook, leaving a lock watch reads as stale.
# So when $PPID is an sh/dash/bash whose command line names this hook, step
# up exactly one level to its parent; otherwise keep $PPID. One level only,
# so it can never overshoot to a terminal shell or a long-lived host. The
# match is on the suffix because the wrapper's argv may still carry an
# unexpanded ${CLAUDE_PLUGIN_ROOT}. Every failed or empty `ps` lookup falls
# back to $PPID silently: this never fails and prints nothing but the pid.
legion_session_pid() {
  local name args parent
  name=$(ps -o comm= -p "$PPID" 2>/dev/null) || name=""
  name="${name##*/}"
  case "${name#-}" in
    sh | dash | bash) ;;
    *)
      printf '%s\n' "$PPID"
      return 0
      ;;
  esac
  args=$(ps -o args= -p "$PPID" 2>/dev/null) || args=""
  case "$args" in
    *hooks/session-lock.sh*) ;;
    *)
      printf '%s\n' "$PPID"
      return 0
      ;;
  esac
  parent=$(ps -o ppid= -p "$PPID" 2>/dev/null) || parent=""
  parent="${parent//[[:space:]]/}"
  case "$parent" in
    '' | *[!0-9]*) printf '%s\n' "$PPID" ;;
    *) printf '%s\n' "$parent" ;;
  esac
  return 0
}

# Passing the pid explicitly matters: without --pid the CLI falls back to its
# own process id, which is this short-lived hook and is dead moments later,
# so the lock would read as stale immediately.
SESSION_PID=$(legion_session_pid)
("$LEGION" watch session-start --repo "$REPO" --pid "$SESSION_PID" >/dev/null 2>&1 &)

exit 0
