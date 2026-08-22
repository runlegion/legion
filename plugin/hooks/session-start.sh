#!/bin/bash
# Legion SessionStart hook: focused bootstrap.
#
# Session-only side effects live here: marker cleanup, index warm, daemon
# supervisor, watch lock. The banner itself -- identity, operating
# contract, pending replies, checkpoint, index status, current work,
# autonomy budget -- is assembled by lib/boot-sections.sh (#879), the SAME
# driver post-compact.sh calls, so the two SessionStart matchers cannot
# drift out of sync the way they did before #879.
#
# Everything else (bulk recall, surface, bullpen) is pulled on demand
# during the session via recall/consult/bullpen commands.
#
# Also warms the Tantivy index in the background so the first PreToolUse
# recall hit is fast (cold ~2.2s, warm ~170ms).

# shellcheck source=lib/prelude.sh
source "${CLAUDE_PLUGIN_ROOT:-}/hooks/lib/prelude.sh" 2>/dev/null || exit 0
# shellcheck source=lib/emit.sh
source "${CLAUDE_PLUGIN_ROOT:-}/hooks/lib/emit.sh" 2>/dev/null || exit 0
# NOT `|| exit 0` like prelude/emit above. Those are hard dependencies;
# boot-sections.sh supplies only the banner, and this hook's session-only
# side effects below (marker cleanup, index warm, daemon supervisor, watch
# lock, GitHub sync) need none of it. Exiting here on a missing lib would
# skip all five as collateral -- they had no such dependency before #879.
# shellcheck source=lib/boot-sections.sh
source "${CLAUDE_PLUGIN_ROOT:-}/hooks/lib/boot-sections.sh" 2>/dev/null || true

LOG="$LEGION_HOOK_LOG"

legion_hook_parse || exit 0

if [ -z "$CWD" ]; then
  exit 0
fi

# Clean up per-session markers from prior session. Reset both -- the Stop
# hook gates its reflect prompt on the work marker, which is touched by the
# PostToolUse mark-work hook on any actual tool use. Sessions with only prose
# Q&A (no tools) skip the reflect prompt. See #339 cleanup batch.
CWD_HASH=$(legion_hash_str "$CWD")
rm -f "/tmp/legion-reflected-${CWD_HASH}" 2>/dev/null
rm -f "/tmp/legion-work-${CWD_HASH}" 2>/dev/null

# Warm the Tantivy index in the background
("$LEGION" recall --repo "$REPO" --context warmup --limit 1 >/dev/null 2>&1 &)

# Dashboard daemon supervisor (#321): probe /health, (re)spawn as needed.
# Backgrounded with stdin closed so SessionStart latency does not include
# the curl probe or the legion serve spawn handshake.
(bash "${CLAUDE_PLUGIN_ROOT}/hooks/_legion-daemon-supervisor.sh" >/dev/null 2>&1 < /dev/null &)

# The interactive-session lock moved to its own hook, session-lock.sh (#996).
# It has to fire on `resume` and `clear` as well as `startup`, and this
# script must not -- a resumed transcript already carries the boot banner.
# Different matchers, different scripts.

# #931: the GitHub-issues-into-kanban sync (`legion sync`, 5s timeout,
# opt-out via LEGION_NO_SYNC=1) that used to run here is gone with the
# card surface -- `legion work`/boot_section_work source live from the
# work source on each call (see src/queue.rs's module doc comment), so
# there is no local cache left to keep warm.

# Banner assembly: identity, operating contract, pending replies,
# checkpoint, index status, current work, autonomy budget -- in that order
# (LEGION_BOOT_SECTIONS in lib/boot-sections.sh). See that file for why the
# order is fixed and why no per-hook section list lives here anymore.
# Guarded: the source above is deliberately non-fatal, so a missing lib
# costs the banner, not the side effects that already ran.
OUTPUT=""
if command -v emit_boot_core >/dev/null 2>&1; then
  OUTPUT=$(emit_boot_core)
fi

if [ -n "$OUTPUT" ]; then
  emit_context "SessionStart" "$OUTPUT"
fi
