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

# $PPID is the Claude session process -- the long-lived pid the lock must
# track. Passing it explicitly matters: without --pid the CLI falls back to
# its own process id, which is this short-lived hook and is dead moments
# later, so the lock would read as stale immediately.
("$LEGION" watch session-start --repo "$REPO" --pid "$PPID" >/dev/null 2>&1 &)

exit 0
