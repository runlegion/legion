#!/bin/bash
# Legion SessionEnd hook (#673 fix 2): remove the interactive .session lock
# when a session actually terminates.
#
# The SessionStart hook writes a `.session` lock (legion watch session-start,
# tracking the long-lived Claude session pid) so the watch daemon does not
# spawn a duplicate agent while a human session is open (#583). Until now the
# only cleanup was passive -- the daemon's active-session gate deletes the
# `.session` opportunistically on a later poll when it reads a dead pid. That
# leaves a window where a recycled pid reads as a false "active session" and
# suppresses a legitimate wake. This hook closes the window by removing the
# lock the moment the session ends.
#
# SessionEnd fires on real session termination (NOT per-turn like Stop), so it
# is safe to remove the `.session` here -- the long-lived process is exiting.
# Idempotent and fail-open: a missing file, absent CLI, or any error is a
# no-op (|| true), so a hook failure can never affect shutdown.
#
# For watch-spawned wakes $LEGION_WAKE_ATTEMPT_ID is set and also stamps
# exit_observed_at for the reaper; for interactive sessions it is empty and the
# --repo hint drives the `.session` removal.

# shellcheck source=lib/prelude.sh
source "${CLAUDE_PLUGIN_ROOT:-}/hooks/lib/prelude.sh" 2>/dev/null || exit 0

legion_hook_parse || exit 0

# Only act on covered repos -- we need a known repo name to resolve the
# `.session` path, and uncovered repos have no lock to clear.
legion_hook_covered || exit 0
if [ -z "$REPO" ] || [ ! -x "$LEGION" ]; then
  exit 0
fi

"$LEGION" watch session-end \
  --attempt-id "${LEGION_WAKE_ATTEMPT_ID:-}" \
  --repo "$REPO" \
  >/dev/null 2>&1 || true

exit 0
