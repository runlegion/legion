#!/bin/bash
# post-release-cleanup.sh
# Run after releasing a new legion version to ensure you're using the clean release.
# Kills debug processes, removes shims, restarts daemon on the released binary.

set -e

echo "[cleanup] Stopping any debug legion processes..."
pkill -f "target/debug/legion" 2>/dev/null && echo "  killed debug processes" || echo "  no debug processes running"
pkill -f "target/release/legion" 2>/dev/null && echo "  killed release processes" || echo "  no release processes running"

echo "[cleanup] Stopping daemon..."
DAEMON_PID_FILE="${HOME}/Library/Application Support/legion/daemon.pid"
if [ -f "$DAEMON_PID_FILE" ]; then
    PID=$(cat "$DAEMON_PID_FILE")
    if kill -0 "$PID" 2>/dev/null; then
        kill "$PID" && echo "  daemon stopped (pid $PID)"
        rm -f "$DAEMON_PID_FILE"
    fi
fi

echo "[cleanup] Removing local plugin shims..."
# The plugin/ directory in the repo can shadow the cached release
SHIM_MARKER="/tmp/legion-using-local-plugin"
if [ -f "$SHIM_MARKER" ]; then
    rm -f "$SHIM_MARKER"
    echo "  removed shim marker"
fi

echo "[cleanup] Clearing old plugin cache versions..."
CACHE_DIR="${HOME}/.claude/plugins/cache/legion/legion"
if [ -d "$CACHE_DIR" ]; then
    LATEST=$(ls -1 "$CACHE_DIR" | sort -V | tail -1)
    for VERSION in $(ls -1 "$CACHE_DIR"); do
        if [ "$VERSION" != "$LATEST" ]; then
            echo "  removing old version: $VERSION"
            rm -rf "${CACHE_DIR}/${VERSION}"
        fi
    done
    echo "  keeping latest: $LATEST"
fi

echo "[cleanup] Verifying legion on PATH..."
LEGION_PATH=$(which legion 2>/dev/null || echo "")
if [ -n "$LEGION_PATH" ]; then
    VERSION=$(legion --version 2>/dev/null || echo "unknown")
    echo "  $LEGION_PATH ($VERSION)"
else
    echo "  WARNING: legion not on PATH"
fi

echo "[cleanup] Restarting daemon on released binary..."
if [ -n "$LEGION_PATH" ]; then
    legion daemon-spawn --port 3131 && echo "  daemon started" || echo "  daemon start failed"
fi

echo "[cleanup] Done."
