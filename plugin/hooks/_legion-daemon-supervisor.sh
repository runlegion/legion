#!/bin/bash
# Legion dashboard daemon supervisor (#321).
#
# Probes `GET http://localhost:3131/health`. Acts on three outcomes:
#
#   1. Healthy + version matches local binary:  silent no-op.
#   2. Unreachable:                              spawn `legion serve` detached.
#   3. Healthy but version mismatch:             kill stale daemon by PID,
#                                                spawn fresh.
#
# Idempotent across concurrent session starts. If the port is already
# bound by another supervisor's spawn, the second `legion serve` fails
# fast and the supervisor treats that as success (someone else got there
# first).
#
# Fail-open: any probe error, missing curl, missing legion binary, or
# pidfile parse failure exits 0 silently. The supervisor never blocks
# SessionStart on infrastructure.
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
STATE_DIR="${XDG_STATE_HOME:-$HOME/.local/state}/legion"
PIDFILE="${STATE_DIR}/daemon.pid"

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

# Spawn helper: detach via setsid (Linux) or nohup (mac). Both available
# on macOS bash; setsid is Linux-only so we try it then fall back.
spawn_daemon() {
  local reason="$1"
  if command -v setsid >/dev/null 2>&1; then
    setsid "$LEGION_BIN" serve >/dev/null 2>>"$LOG" < /dev/null &
  else
    nohup "$LEGION_BIN" serve >/dev/null 2>>"$LOG" < /dev/null &
  fi
  disown 2>/dev/null || true
  echo "[legion-daemon-supervisor] daemon: ${reason} (pid $!)" >> "$LOG"
}

# Kill helper: only kills if the PID exists AND its argv contains
# 'legion serve' -- defensive against a stale pidfile pointing at a
# reused PID belonging to an unrelated process.
kill_stale_daemon() {
  local pid="$1"
  if [ -z "$pid" ] || ! [ "$pid" -gt 0 ] 2>/dev/null; then
    return
  fi
  if ! kill -0 "$pid" 2>/dev/null; then
    return
  fi
  # ps -o command returns the full argv on macOS + Linux. Filter for
  # the literal 'legion serve' so a recycled PID owned by something
  # else is never the target.
  if ps -p "$pid" -o args= 2>/dev/null | grep -q 'legion serve'; then
    kill "$pid" 2>/dev/null
    # Give the graceful shutdown a beat to remove the pidfile and free
    # the port. SIGTERM is enough; supervisor does not escalate.
    sleep 1
  fi
}

# Probe /health. Short timeout (2s) since this runs in the SessionStart
# background path -- waiting longer is just buffering before we accept
# "not healthy" anyway.
RESPONSE=$(curl --silent --max-time 2 "$HEALTH_URL" 2>/dev/null)
CURL_RC=$?

if [ "$CURL_RC" -ne 0 ] || [ -z "$RESPONSE" ]; then
  # Unreachable: connection refused, timeout, or empty body. Spawn fresh.
  spawn_daemon "started fresh (no response from $HEALTH_URL)"
  exit 0
fi

# Reachable. Parse the version field. Malformed JSON -> treat as unhealthy
# rather than guessing (fail-closed on suspect responses, fail-open on
# transport errors).
DAEMON_VERSION=$(echo "$RESPONSE" | jq -r '.version // empty' 2>/dev/null)
if [ -z "$DAEMON_VERSION" ]; then
  echo "[legion-daemon-supervisor] /health returned malformed JSON; respawning" >> "$LOG"
  if [ -f "$PIDFILE" ]; then
    kill_stale_daemon "$(cat "$PIDFILE" 2>/dev/null)"
  fi
  spawn_daemon "respawn (malformed health response)"
  exit 0
fi

if [ "$DAEMON_VERSION" = "$LOCAL_VERSION" ]; then
  # Healthy + matching: silent.
  exit 0
fi

# Version mismatch. Who answers the port decides the remedy (#613,
# absorbed #601): the daemon owns :3131 while it runs, and replacing it
# with a `legion serve` would silently drop the watch loop. /health now
# reports a `role` field; a daemon gets bounced in place via
# daemon-restart (which owns the stop/spawn dance against the daemon's
# own pidfile). Pre-#613 binaries report no role -- they can only be a
# `legion serve`, so the kill+spawn path below keeps working for them.
DAEMON_ROLE=$(echo "$RESPONSE" | jq -r '.role // empty' 2>/dev/null)
if [ "$DAEMON_ROLE" = "daemon" ]; then
  echo "[legion-daemon-supervisor] daemon v${DAEMON_VERSION} != local v${LOCAL_VERSION}; restarting daemon in place" >> "$LOG"
  "$LEGION_BIN" daemon-restart >/dev/null 2>>"$LOG"
  exit 0
fi

# Role "serve" (or absent): kill stale serve, spawn fresh.
echo "[legion-daemon-supervisor] daemon v${DAEMON_VERSION} != local v${LOCAL_VERSION}; replacing" >> "$LOG"
if [ -f "$PIDFILE" ]; then
  kill_stale_daemon "$(cat "$PIDFILE" 2>/dev/null)"
fi
spawn_daemon "replaced stale v${DAEMON_VERSION} -> v${LOCAL_VERSION}"
exit 0
