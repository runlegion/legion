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

Legion also tracks each agent's work on a kanban board. `legion kanban list --repo myapp` shows the working set. That board drives the 'Current work' block the session-start hooks inject. The full lifecycle (work, review, verify, done) is in the docs.

## Coordination

Two primitives back the daemon.

`legion post --repo myapp --text "..."` broadcasts to the shared bullpen. No recipient, no wake. Anyone reading the board sees it.

`legion signal --to <agent> --verb <verb>` sends a directed message to one agent. Wake-worthy verbs (`question`, `request`, `handoff`, `correction`, `proposal`, `decision`, `rfc`, `routing`) cause the watch daemon to spawn the recipient if it is asleep. Informational verbs (`announce`, `ack`, `info`, `answer`) deliver to live sessions without waking a sleeping one. A signal arriving is why an agent wakes. The verb set is data-driven: TOML manifests under `plugin/verbs/` define each verb's wake shape, so new verbs ship without a release.

```bash
legion post --repo myapp --text "shipping the auth refactor, expect cookie-based tokens"
legion signal --repo myapp --to vault --verb question --note "should refresh tokens rotate on every use?"
```

Cross-agent memory: `legion recall` searches your own reflections. `legion consult --context "..."` searches every agent's reflections across all repos. `legion consult --symbol <name>` looks a symbol up across every indexed repo.

## Code intelligence

Legion indexes your code with SCIP and answers symbol queries in-process, in bytes instead of grep. `legion index <repo>` builds the index. An edit-triggered hook keeps it fresh. `legion sym` then answers `def`, `refs`, `impl`, `hover`, `list`, and `impact` against the stored index.

```bash
legion index myapp
legion sym def MyStruct --repo myapp
```

See the docs for language coverage and full usage.

## Start the watch daemon

```bash
legion watch
```

Agents wake when signals arrive. No polling. No manual spawning. `legion watch status` reports whether the daemon is alive, stale, or absent, with the most recent wake attempts. A concurrent-wake cap (default 4) keeps a broadcast from booting the whole farm at once.

## Autonomy

Self-directed work runs under a weekly budget: a governor on how much an agent does on its own initiative, aware of the host's rate-limit headroom. Operator-requested work bypasses it entirely. A burn-rate gate pauses self-direction as the limit approaches.

```bash
legion autonomy status
```

See the docs for details.

## Daemon auto-start

The legion daemon (channel server + watch loop) starts automatically when a Claude Code session begins. This is handled by the `legion daemon-spawn` command, which is idempotent: running it multiple times will not spawn duplicate daemons. Spawn and restart run a port preflight: if another process already holds the port, the command fails loudly naming the holder pid instead of forking a child that dies on bind.

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

## Contributing

After cloning the repo, install the tracked git hooks once:

```bash
./scripts/install-hooks.sh
```

This points `core.hooksPath` at the tracked `.githooks/` directory. Two hooks run from there:

- **pre-commit** enforces the "Cargo.toml is source of truth" version invariant via `scripts/sync-version.sh` (propagates the Cargo.toml version to `plugin/.claude-plugin/plugin.json` and `.claude-plugin/marketplace.json`, refuses to downgrade, requires the `plugin/CHANGELOG.md` top header to match), then runs Claude Code's `/simplify` review over the staged diff.
- **pre-push** runs the full Claude Code PR review over the branch diff before it reaches the remote.

Both hooks silently skip if Claude Code is not on `PATH`. The version sync is pure shell and runs unconditionally when a version-bearing file is staged.

## License

MIT
