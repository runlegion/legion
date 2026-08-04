# Legion hooks: the boundary, and what each guard is for

Issue: #860. Companion to `lib/prelude.sh`, which carries the short form of this
boundary in its header because every hook sources it.

## The boundary

**Claude-layer hooks fire on the AGENT'S tool call only. They never see child
processes.** A script, a Makefile, a test suite, a `bash -c` wrapper, or any tool
that shells out executes unimpeded. The same is true of the harness's
`permissions.deny` ruleset: it is evaluated against the Bash command the agent
submits, not against what that command goes on to spawn.

This is REQUIRED by design, not a defect awaiting a patch. `no-git-push.sh`'s
header states the reason:

> No recursion guard is needed: hooks fire on the AGENT's Bash tool calls, not on
> child processes, so `legion push`'s own internal `git push` never re-enters
> here. That is precisely why this is a hook and not a PATH shim or a symlink --
> legion is itself a git consumer, and a shim would recurse.

A PATH shim that intercepted every `git` would intercept legion's own `git`
invocations and recurse. The interception point has to be the agent's tool call,
and the agent's tool call is exactly one process deep.

### The consequence

**Claude-layer hooks SHAPE AGENT BEHAVIOUR. They are not an enforcement
boundary.** Any rule that must hold even when an agent runs a script has to live
somewhere the script cannot step around:

- the **git layer** -- `.githooks/*`, installed via `core.hooksPath`, which git
  itself runs regardless of who invoked git;
- the **binary** -- e.g. `REFUSED_BRANCHES` in `src/cli/push.rs:20`, which holds
  for every caller of `legion push`;
- **remote branch protection** -- the only layer that survives a compromised or
  unconfigured local checkout entirely.

Each of those has its own limits, recorded per row in the audit below. Naming a
layer is not the same as that layer being sufficient.

## Evidence

Legend used here and in the table:

- `[REPRO]` -- reproduced in-session; the command and its output are in
  Evidence 1 below.
- `[INCIDENT]` -- observed during the 2026-08-04 main-corruption incident;
  written up in reflection `019fce1a-c597-7f63-a54d-7a3018ce6fba`.
- `[CODE]` -- read directly off the source at the cited `file:line`.
- `[UNVERIFIED]` -- asserted but not confirmed here; treat as an open question.

### 1. A denied command runs when it is one process deeper `[REPRO]`

Reproduced 2026-08-04 in a session where a direct shell `grep` is denied. The
control and the escape, verbatim:

```
# Control -- the agent submits grep as its Bash command:
$ grep -c '^' plugin/hooks/lib/prelude.sh
Permission to use Bash with command grep -c '^' ... has been denied.

# Escape -- identical grep, one process deeper:
$ cat > /tmp/repro.sh <<'EOF'
#!/bin/bash
grep -c '^' plugin/hooks/lib/prelude.sh
EOF
$ bash /tmp/repro.sh
119
```

Same binary, same arguments, same session, same repo. The only difference is who
executed it. That is the whole boundary in two commands, and it is the form to
re-run when you want to check whether the boundary has moved.

The original 2026-08-04 reproduction used `gh --version` inside a shell script:
it executed with no interception although a direct `gh` call is denied by
`no-gh.sh` `[INCIDENT]`. The `grep` form above is preferred for re-running --
it is read-only, needs no network, and gives a checkable numeric answer.

### 2. A push from a test fixture reached GitHub `[INCIDENT]`

`scripts/test-release.sh` drove git fixtures with `(cd "$WT" && git ...)`. A
mutation made the path helper return empty; `cd ""` is a silent no-op in bash, so
the fixture's `git push -u origin main` ran against the real repository and
reached GitHub, rewriting `main`. `no-git-push.sh` never saw it -- the push came
from a script, not from a tool call. Fixture sandboxing is #861; the git-layer
refusal is #859.

Do not re-run this one.

## Per-guard audit

Every hook in this directory that emits `emit_deny`, `emit_rewrite`, or
`emit_block` is listed. The list was built by searching for those three
emitters across `plugin/hooks/`, not from memory. Two inject-only hooks
(`pre-script-search.sh`, `recall-first.sh`) are listed as well, because the
question "is this enforced?" is asked of them anyway.

The hooks deliberately absent from the table -- `session-start.sh`,
`session-end.sh`, `post-compact.sh`, `mark-work.sh`, `post-edit-index.sh`,
`post-task-state.sh`, `identity-chain-load.sh`, `uncertainty-emit-on-task.sh`,
`uncertainty-witness-on-completion.sh`, `setup-binary.sh`, and the `_legion-*.sh`
sourced helpers -- refuse nothing at all. There is no verdict to record for a
hook with no refusal in it.

**ADVISORY** -- the guard routes, nudges, or controls cost. An agent that
escapes it via a script has spent tokens badly or skipped an audit row, not
broken anything. The escape is an acceptable cost.

**MUST-BE-TOTAL** -- a script escape is a real hole: it corrupts state, destroys
work, or defeats a control the project relies on.

| Hook | Event / matcher | Action | Verdict | What actually enforces it today |
|---|---|---|---|---|
| `no-git-push.sh` | PreToolUse / Bash | rewrite to `legion push`; deny untranslatable flags | **MUST-BE-TOTAL** | **Partially covered.** `.githooks/pre-push` is the only layer that sees a script's push (`git config core.hooksPath` resolves to `.githooks` in this checkout and its worktrees, checked 2026-08-04), but it **exempts `main`/`master` by name** (`.githooks/pre-push:52-55` `[CODE]`), reads the CWD's current branch rather than the refs on stdin, and exits 0 when the reviewer is unavailable. `REFUSED_BRANCHES` (`src/cli/push.rs:20` `[CODE]`) only fires when something calls `legion push` -- a script calling `git push` never reaches it. Remote branch protection: `[UNVERIFIED]` here; the 2026-08-04 incident is direct evidence that a push to `refs/heads/main` was accepted by the remote at that time `[INCIDENT]`. **Fix in flight: #859.** |
| `no-direct-db.sh` | PreToolUse / Bash | deny any command naming `legion.db` | **Split.** Reads: ADVISORY. **Writes: MUST-BE-TOTAL** | **Nothing enforces the write case.** `legion.db` is a plain user-owned SQLite file; a script running `sqlite3` writes to it with no barrier. There is no `PRAGMA user_version` guard, no integrity probe, and `PRAGMA foreign_keys` is not enabled globally (`src/db/kanban.rs:109`, `src/db/wake.rs:155` `[CODE]`), so the schema does not constrain an out-of-band writer either. `legion health` samples machine load, not database integrity (`src/health.rs:7-26` `[CODE]`). The hook's own header already concedes an in-band gap (`ATTACH` from an interactive `sqlite3` prompt) and calls the mitigation "cultural, not technical". **No total layer exists and no issue is filed for one -- see the section below.** |
| `no-gh.sh` | PreToolUse / Bash | deny, with a translated `legion` command | ADVISORY | Nothing, by design. `legion` implements a subset of `gh` with different ergonomics, so a rewrite would silently drop flags (#828) and a total block would refuse work legion cannot do. The cost of an escape is a missing `legion audit` row, not a broken repo. Sharp edge: the destructive sub-cases (`gh pr merge`, `gh api` writing a ref) are only bounded by remote branch protection, `[UNVERIFIED]` above. |
| `no-local-memory.sh` | PreToolUse / Write, Edit, MultiEdit | deny writes under `~/.claude/projects/*/memory/` | ADVISORY | Nothing. A file written there is invisible to other agents and repos -- the cost of an escape is knowledge stranded in one session, not damage. See "matcher gaps" below: this one does not need a script to escape. |
| `no-harness-explore.sh` | PreToolUse / Agent, Task | deny `subagent_type: Explore`, redirect to `legion:legion-explore` | ADVISORY | Nothing, and nothing is needed: subagents are spawned through the Agent/Task tool, so there is no shell form of this call for a script to carry. The child-process boundary does not apply. |
| `pre-grep.sh` | PreToolUse / Grep, Glob | deny when `sym def` resolves the pattern locally; else inject | ADVISORY | Operator `permissions.deny` is the mandatory tier (`docs/decisions/2026-06-02-grep-blocking-is-operator-permissions.md`) -- but it is Claude-layer too, so a script-shaped search escapes it identically `[REPRO]`. Advisory either way: this guard controls token cost and routing, not correctness. |
| `pre-bash-grep.sh` | PreToolUse / Bash | deny when `sym def` resolves the pattern locally; else inject | ADVISORY | Same as `pre-grep.sh`. Its own header already describes itself as "SOFT FALLBACK GUIDANCE". |
| `pre-read-sym.sh` | PreToolUse / Read | deny unbounded Reads of large indexed source files | ADVISORY | Nothing, correctly: it is a cost guard with a telemetried `LEGION_BYPASS_READ=1` escape by design. |
| `pre-script-search.sh` | PreToolUse / Bash, Write, Edit | inject only -- never denies | ADVISORY by explicit design | Nothing. Its header states the posture: "INJECT, NEVER DENY. A script that searches may also do real work." |
| `recall-first.sh` | PreToolUse / WebFetch, WebSearch | inject only -- never denies | ADVISORY | Nothing needed. |
| `stop.sh` | Stop | block the session from stopping on Accepted or dead-delegated cards | Boundary **N/A** | Not tool-scoped. A Stop hook fires on the session lifecycle, so there is no child process to escape through. Its escape is the declared `LEGION_SKIP_STOP_BLOCK=1`, which is telemetried. |
| `precompact.sh` | PreCompact | block compaction with a user-facing reason | Boundary **N/A** | Not tool-scoped, same as `stop.sh`. |

### MUST-BE-TOTAL guards with no enforcing layer today

Stated plainly, because the temptation is to write down the layer we mean to
build:

1. **Direct writes to `legion.db` have no enforcing layer at all.** Not the
   binary, not the schema, not the filesystem. `no-direct-db.sh` is the only
   thing standing between an agent and the database, and it is one process deep.
   No issue is filed for this; #860 is where it was found.
2. **`git push` to `main` is only partially covered**, and the covering layer
   exempts exactly the branch that matters. #859 fixes the git layer; whether
   remote branch protection is configured is unverified and is an operator
   question, not a code question.

## Pre-existing matcher gaps (a different failure mode)

These are guards whose *matcher* misses shapes it means to catch. They are not
the child-process boundary and should not be conflated with it -- a matcher gap
is fixable in the hook; the child-process boundary is not.

- **`no-local-memory.sh` matches Write/Edit/MultiEdit only.** A plain
  `echo ... > ~/.claude/projects/x/memory/MEMORY.md` from the Bash tool escapes
  it with no script involved.
- **`no-gh.sh` matches the first token only.** `cd /tmp && gh pr merge ...` or
  `env gh ...` escapes it directly. The class is documented in reflection
  `019e27f2-afc1-7522-b299-f224d04aae2c`; the absolute-path case
  (`/opt/homebrew/bin/gh`) *is* handled, via basename.

## Telemetry caveat

**`bypass.jsonl` cannot record what the hooks never saw.** Every bypass row is
written by a hook, from inside a hook invocation, after the hook fired. A
script-shaped escape produces no row -- not because it was allowed, but because
nothing was asked.

So: **absence of bypass rows is not absence of bypass.** `legion telemetry
summary` counts escapes through the sanctioned door only. `legion telemetry
etc-summary` -- the #704 epic's primary success metric -- reads `etc-usage.jsonl`
and counts sanctioned `sym etc` calls, so a search that happened in a script
raises neither number: it is not a recorded bypass and it is not recorded usage.
An escape route we cannot see reads as adoption in both.

This holds for `permissions.deny` too: a harness denial that never fires because
the command was one process deeper is indistinguishable, in the data, from a
command that was never issued.

Both caveats are restated in `legion telemetry summary --help` and
`legion telemetry etc-summary --help`, where the counts are actually read.

## Adding a hook

Record a verdict in the table above in the same change that adds the hook. The
question to answer is not "is this important?" but:

> If an agent runs a script that does this, is that an acceptable outcome?

If the answer is no, the hook is not the whole answer, and the change is not
finished until a git-layer, binary-layer, or remote implementation exists -- or
an issue for one is filed and linked in the row.
