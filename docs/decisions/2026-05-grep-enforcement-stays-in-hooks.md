# Grep-enforcement stays in hooks; allowlists guard command/skill surfaces

Date: 2026-05-30
Issue: #530 (refactor: replace grep-enforcement hooks with skill disallowed-tools where viable)
Parent epic: #513 (harness embraces -- Opus 4.8 / CC v2.1.154)

## Question

CC v2.1.152 added `disallowed-tools` frontmatter (remove a tool while a skill/command is
active). #530 asked: can we move grep-enforcement out of the always-on PreToolUse hooks and
into declarative frontmatter, shrinking the hook stack without weakening enforcement?

## Audit

The four grep-enforcement hooks were read in full and classified:

| Hook | Tool(s) | Behavior | Can it move to frontmatter? |
|------|---------|----------|------------------------------|
| `pre-grep-recall.sh` | Grep, Glob | **Never blocks.** Runs `legion recall`, injects hits as `additionalContext`, always `allow`. | No -- `disallowed-tools` removes the tool; there is no block here to replace, and removal would lose the injection. |
| `pre-grep-scip.sh` | Grep, Glob | **Never blocks.** Runs `legion sym def`, injects hits, always `allow`. | No -- same: it is an injector, not a blocker. |
| `pre-bash-grep.sh` | Bash | **Dynamic ladder.** Blocks only when the extracted pattern is symbol-shaped AND the repo is SCIP-indexed AND `sym def` returns a *local* hit; soft-bypass (refused on local symbol hits) and hard-bypass tiers. | No -- frontmatter cannot express "block `grep` only when the pattern resolves to a local symbol." Static removal of Bash is absurd. |
| `pre-read-sym.sh` | Read | **Dynamic ladder.** Blocks only when the path is a source extension AND indexed AND the file is >500 lines AND no small `limit` is set. | No -- frontmatter cannot express "block Read only on large unbounded source reads." |

**Conclusion: none of the four hooks is static, unconditional grep-blocking.** Two are pure
injectors (allow + context); two are stateful conditional ladders. `disallowed-tools` is a
static, unconditional tool removal and can express none of them without weakening enforcement
(losing the injected sym/recall context, the size/symbol conditions, and the bypass ladder).
AC #2 ("dynamic ladder/bypass retained in hooks where frontmatter can't express it") is therefore
satisfied by retaining all four hooks unchanged.

## Where declarative tool-restriction already lives

The premise that grep-restriction was missing from command/skill frontmatter was already false:
legion surfaces use `allowed-tools` (an **allowlist**, strictly stronger than the `disallowed-tools`
denylist #530 proposed).

- 8 of 9 `plugin/commands/*.md` declare `allowed-tools: ["Bash"]` -- Grep/Glob/Read already
  unavailable while they run. (`migrate-memory` is `Bash, Read, Write`; still no Grep/Glob.)
- `plugin/skills/legion-memory/SKILL.md` was already `Bash, Read`.

These surfaces were built right. There is nothing to "move" into them.

## The one real change

`plugin/skills/legion-simplify/SKILL.md` declared `allowed-tools: Bash, Read, Glob, Grep`. The
simplify skill's core question -- "is this logic duplicated elsewhere / does this helper already
exist?" -- is a `legion sym` / `legion recall` question by doctrine, not a grep. Granting Grep/Glob
invited the exact anti-pattern the doctrine forbids. Tightened to `allowed-tools: Bash, Read`.
Bash remains, so a genuinely necessary scan still routes through `pre-bash-grep`'s sym ladder.

## Parity (AC #3 -- no enforcement weakening)

- The dynamic symbol-block is already pinned by `plugin/hooks/test-pre-bash-grep.sh:170-173`
  ("indexed repo + symbol-shape pattern with LOCAL-REPO hit -> BLOCK"). Because #530 does not
  touch the hooks, that guarantee holds by construction.
- New: `tests/integration.rs::legion_command_and_skill_surfaces_never_grant_grep` asserts no
  legion command/skill allowlist grants Grep/Glob -- so a future "just grep here" edit fails CI
  rather than silently weakening the doctrine. This is the parity lock for the surface that
  actually changed (the allowlists), and it runs in CI (the `plugin/hooks/test-*.sh` scripts do not).

## Unaddressed-but-noted: the subagent surface

PreToolUse hooks do not fire on subagents, so Explore/general-purpose subagents grep unenforced.
`disallowed-tools`/`tools` on agent definitions is the only mechanism there. That is agent-def
work (shared config, separate from this hook/command refactor) and is tracked separately; it is
out of scope for #530, whose named file locations are hooks + commands.
