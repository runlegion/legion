# Legion

Self-hosted orchestrator for AI coding agents. Memory, coordination, autonomy.

Written in Rust. Free forever. No cloud required.

## Install

```bash
/plugin marketplace add runlegion/legion
/plugin install legion
```

## First session

Legion installs hooks that run automatically. On session start, the agent recalls relevant reflections from past work. On session end, the agent reflects on what it learned. Over time, the agent builds expertise specific to your codebase.

```bash
legion reflect --repo myapp --text "auth middleware expects refresh tokens in httpOnly cookies, not headers"
legion recall --repo myapp --context "auth token handling"
```

## Start the watch daemon

```bash
legion watch
```

Agents wake when signals arrive. No polling. No manual spawning.

## Daemon auto-start

The legion plugin automatically starts the daemon in the background when a new
Claude Code session begins. No manual setup is required.

The daemon runs the channel server (port 3131) and the watch loop. It is
started from `setup-binary.sh`, not `session-start.sh`, because session-start
output is captured for Claude Code context injection and a background fork
would corrupt the JSON envelope.

Log file locations:

- macOS: `~/Library/Logs/legion/daemon.log`
- Linux: `${XDG_STATE_HOME:-$HOME/.local/state}/legion/daemon.log`

To suppress auto-start (for CI environments or minimal setups that do not need
the dashboard or auto-wake):

```bash
export LEGION_NO_DAEMON=1
```

To start the daemon manually:

```bash
legion daemon-spawn
```

The spawn command is idempotent: if a daemon is already running it prints
`"legion daemon already running (pid N)"` and exits 0.

## Docs

Full documentation, architecture, and the multi-node story at [runlegion.dev](https://runlegion.dev).

## License

MIT
