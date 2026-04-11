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

The legion daemon (channel server + watch loop) starts automatically when a Claude Code session begins. This is handled by the `legion daemon-spawn` command, which is idempotent: running it multiple times will not spawn duplicate daemons.

**Log file location:**
- macOS: `~/Library/Logs/legion/daemon.log`
- Linux: `${XDG_STATE_HOME:-$HOME/.local/state}/legion/daemon.log`

**Opt-out:**

If you prefer to run the daemon manually (e.g., in a dedicated tmux pane for log tailing), set:

```bash
export LEGION_NO_DAEMON=1
```

in your shell or in the environment where Claude Code runs. This disables the auto-start entirely. The daemon can still be started manually with `legion daemon --port 3131`.

## Daemon auto-start

The legion daemon (channel server + watch loop) starts automatically when a Claude Code session begins. This is handled by the `legion daemon-spawn` command, which is idempotent: running it multiple times will not spawn duplicate daemons.

**Log file location:**
- macOS: `~/Library/Logs/legion/daemon.log`
- Linux: `${XDG_STATE_HOME:-$HOME/.local/state}/legion/daemon.log`

**Opt-out:**

If you prefer to run the daemon manually (e.g., in a dedicated tmux pane for log tailing), set:

```bash
export LEGION_NO_DAEMON=1
```

in your shell or in the environment where Claude Code runs. This disables the auto-start entirely. The daemon can still be started manually with `legion daemon --port 3131`.

## Docs

Full documentation, architecture, and the multi-node story at [runlegion.dev](https://runlegion.dev).

## License

MIT
