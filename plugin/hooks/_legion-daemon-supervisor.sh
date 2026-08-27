#!/bin/bash
# Legion daemon supervisor (#321, #997).
#
# Probes `GET http://localhost:3131/health`. Acts on these outcomes:
#
#   1. Healthy + version and build match local binary:  silent no-op.
#   2. Unreachable:                                      spawn `legion
#      daemon-spawn` detached (cold start).
#   3. Healthy but version drift, build drift (same
#      version), or a malformed health response:          `legion
#      daemon-restart`, regardless of role -- a legacy `serve` process or a
#      pre-#613 binary reporting no role restarts exactly the same way as a
#      `daemon`.
#
# #997: the cold-start path used to run the deprecated `serve` subcommand
# (dashboard-only, no watch loop), which left watch dead on a fresh machine
# until someone started a daemon by hand. It now always goes through
# `daemon-spawn` (cold start) or `daemon-restart` (everything else). Both
# are the sanctioned Rust-side entry points and own pidfile handling,
# orphan-on-port detection, and SIGTERM-then-SIGKILL themselves
# (src/daemon.rs:451-499) -- this script tracks no pidfile and kills
# nothing directly.
#
# Idempotent across concurrent session starts: `daemon-spawn` already no-ops
# when a live daemon holds the pidfile, so the script adds no port-race
# handling of its own.
#
# Fail-open: any probe error, missing curl, missing jq, missing legion
# binary, or a non-zero daemon-spawn/daemon-restart exit stays silent and
# exits 0. The supervisor never blocks SessionStart on infrastructure.
#
# Run by `plugin/hooks/session-start.sh` as a background fire-and-forget
# so it does not add to the SessionStart latency budget.
#
# Skip via LEGION_SKIP_DAEMON_SUPERVISOR=1.

set -u

if [ "${LEGION_SKIP_DAEMON_SUPERVISOR:-}" = "1" ]; then
  exit 0
fi

LEGION_BIN="${CLAUDE_PLUGIN_ROOT}/bin/legion"
LOG=/tmp/legion-hook-errors.log
PORT="${LEGION_SERVE_PORT:-3131}"
HEALTH_URL="http://localhost:${PORT}/health"

# Hard-required dependencies. Missing any -> silent exit (fail-open).
if [ ! -x "$LEGION_BIN" ]; then
  exit 0
fi
if ! command -v curl >/dev/null 2>&1; then
  echo "[legion-daemon-supervisor] curl missing; skipping" >> "$LOG"
  exit 0
fi
if ! command -v jq >/dev/null 2>&1; then
  echo "[legion-daemon-supervisor] jq missing; skipping" >> "$LOG"
  exit 0
fi

# Local binary version. Bakes into the binary via --version output.
LOCAL_VERSION=$("$LEGION_BIN" --version 2>/dev/null | awk '{print $2}')
if [ -z "$LOCAL_VERSION" ]; then
  echo "[legion-daemon-supervisor] could not read local version; skipping" >> "$LOG"
  exit 0
fi

# Local build id (#698): the "(build <id>)" suffix that --version now carries
# ("legion <version> (build <id>)"). Empty for a pre-#698 binary without the
# suffix -- then the same-version build-drift check below is skipped and the
# supervisor falls back to version-only behavior.
LOCAL_BUILD=$("$LEGION_BIN" --version 2>/dev/null | sed -n 's/.*(build \(.*\))$/\1/p')

# Spawn helper: cold start via `legion daemon-spawn` (#997) -- idempotent,
# starts the daemon (channel + watch loop), never the deprecated
# dashboard-only `serve` subcommand. Detach via setsid (Linux) or nohup
# (mac). Both available on macOS bash; setsid is Linux-only so we try it
# then fall back.
spawn_daemon() {
  local reason="$1"
  if command -v setsid >/dev/null 2>&1; then
    setsid "$LEGION_BIN" daemon-spawn >/dev/null 2>>"$LOG" < /dev/null &
  else
    nohup "$LEGION_BIN" daemon-spawn >/dev/null 2>>"$LOG" < /dev/null &
  fi
  disown 2>/dev/null || true
  echo "[legion-daemon-supervisor] daemon: ${reason} (pid $!)" >> "$LOG"
}

# Restart whoever currently answers the port via `legion daemon-restart`
# (#997). Used for version drift, same-version build drift, and a malformed
# health response -- every remedy short of a cold start. `daemon-restart`
# stops the running process (pidfile-based, with orphan-on-port recovery)
# and spawns fresh itself, so this script never tracks a pidfile or sends a
# signal of its own.
restart_daemon() {
  local reason="$1"
  echo "[legion-daemon-supervisor] ${reason}; restarting via daemon-restart" >> "$LOG"
  "$LEGION_BIN" daemon-restart >/dev/null 2>>"$LOG"
}

# Probe /health. Short timeout (2s) since this runs in the SessionStart
# background path -- waiting longer is just buffering before we accept
# "not healthy" anyway.
RESPONSE=$(curl --silent --max-time 2 "$HEALTH_URL" 2>/dev/null)
CURL_RC=$?

if [ "$CURL_RC" -ne 0 ] || [ -z "$RESPONSE" ]; then
  # Unreachable: connection refused, timeout, or empty body. Cold start.
  spawn_daemon "started fresh (no response from $HEALTH_URL)"
  exit 0
fi

# Reachable. Parse the version field. Malformed JSON -> treat as unhealthy
# rather than guessing (fail-closed on suspect responses, fail-open on
# transport errors).
DAEMON_VERSION=$(echo "$RESPONSE" | jq -r '.version // empty' 2>/dev/null)
if [ -z "$DAEMON_VERSION" ]; then
  echo "[legion-daemon-supervisor] /health returned malformed JSON; respawning" >> "$LOG"
  restart_daemon "malformed health response"
  exit 0
fi

if [ "$DAEMON_VERSION" = "$LOCAL_VERSION" ]; then
  # Same version. A rebuild that did not bump Cargo.toml -- the common dev
  # case -- keeps the version but changes the build id (#698), so version
  # alone cannot tell the daemon is stale. Restart when the build ids differ.
  # Act ONLY when BOTH ids are known and concrete: an "unknown" id (git-less
  # build) or an empty id (pre-#698 binary, or no build_id in /health) means
  # "cannot tell" -- fall back to version-only and stay silent rather than
  # risk a restart loop on indeterminate data.
  DAEMON_BUILD=$(echo "$RESPONSE" | jq -r '.build_id // empty' 2>/dev/null)
  if [ -n "$LOCAL_BUILD" ] && [ -n "$DAEMON_BUILD" ] \
     && [ "$LOCAL_BUILD" != "unknown" ] && [ "$DAEMON_BUILD" != "unknown" ] \
     && [ "$LOCAL_BUILD" != "$DAEMON_BUILD" ]; then
    restart_daemon \
      "daemon build ${DAEMON_BUILD} != local build ${LOCAL_BUILD} (same version ${LOCAL_VERSION})"
    exit 0
  fi
  # Same version, same or indeterminate build: healthy, silent.
  exit 0
fi

# Version drift (#613, absorbed #601): restart via daemon-restart regardless
# of role -- a legacy `serve`, or a pre-#613 binary with no role at all,
# restarts the same way as a `daemon`.
restart_daemon "daemon v${DAEMON_VERSION} != local v${LOCAL_VERSION}"
exit 0
