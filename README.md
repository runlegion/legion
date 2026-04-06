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

## Docs

Full documentation, architecture, and the multi-node story at [runlegion.dev](https://runlegion.dev).

## License

MIT
