#!/bin/bash
# Ensure legion serve is running on port 3131.
# Legion's MCP channel plugin depends on this HTTP/SSE backend.
# First agent to wake starts it; subsequent agents find it already running.

PORT=3131

if lsof -i ":${PORT}" -sTCP:LISTEN >/dev/null 2>&1; then
  exit 0
fi

# Start legion serve in background, detached from this session
nohup legion serve --port "${PORT}" >/tmp/legion-serve.log 2>&1 &
disown

# Wait briefly for it to bind
for i in 1 2 3; do
  sleep 1
  if lsof -i ":${PORT}" -sTCP:LISTEN >/dev/null 2>&1; then
    exit 0
  fi
done

echo "legion serve failed to start -- check /tmp/legion-serve.log" >&2
exit 0
