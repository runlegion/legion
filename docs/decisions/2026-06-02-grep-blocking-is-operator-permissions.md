# Grep-blocking is operator `permissions.deny`, not plugin hooks

Date: 2026-06-02
Issue: #560
Supersedes the enforcement half of: 2026-05-grep-enforcement-stays-in-hooks.md (#530)
Verified against: Claude Code v2.1.159

## The problem

Agents grep/find by 30-year habit. Legion tried to stop this with PreToolUse hooks
(`pre-bash-grep.sh` and friends) plus an env-var bypass ladder. It did not work, and the
telemetry showed why on two axes:

1. **The block was optional.** `LEGION_BYPASS_GREP_HARD=1` always allowed. A one-env-var
   escape against a 30-year habit is a speed bump, not a gate. The agents (this one included)
   leaned on it reflexively.
2. **The block didn't reach subagents.** PreToolUse hooks defined in `settings.json` / a
   plugin do **not** fire on a subagent's tool calls. Every spawned agent grepped ungated.

A hook is the wrong tool for a mandatory rule: it is advisory, bypassable, and main-session
only.

## The mechanism that is right

CC v2.1.x has `permissions.deny` in settings.json, and it is everything the hook was not:

- **Evaluated before `allow`** and **cannot be overridden** by the agent (no env-var escape).
- **Inherits to subagents** — one rule covers the main session and every spawned agent.
- **Matches Bash command patterns** (prefix form, e.g. `Bash(grep:*)`), so shell-grep is
  blocked at the harness, before any hook runs, including inside pipes (each subcommand of a
  compound command is checked).

`permissionDecision: "deny"` is also now the documented clean PreToolUse block (the old
implicit exit-2 model).

## The decision

**Mandatory shell-grep blocking is the operator's `permissions.deny`. The plugin does not
(and cannot) enforce it.**

Recommended ruleset, in `~/.claude/settings.json` (user-global, covers every repo) or
`/etc/claude-code/managed-settings.json` (org-wide, unbypassable even by the operator):

```json
{
  "permissions": {
    "deny": [
      "Bash(grep:*)",
      "Bash(rg:*)",
      "Bash(find:*)",
      "Bash(fd:*)",
      "Bash(ag:*)",
      "Bash(ack:*)"
    ]
  }
}
```

**The escape is a tool, not a bypass.** `Grep` and `Glob` are deliberately **not** denied.
They are the supervised path for the one real need sym cannot serve: free-text / non-indexed
content (an `.astro` file has no SCIP index, so `sym` has nothing — the Grep tool searches its
content). The agent never loses access to information sym can't provide; it loses only the
*shell* duplicate that was the habit and the bypass surface. The Grep-tool hooks
(`pre-grep-recall`, `pre-grep-scip`) still inject sym/recall context and redirect symbol
queries to `sym` / `sym list` — but they only redirect when sym has an answer, so for `.astro`
they simply allow. The escape opens exactly when the agent genuinely can't get the info another
way.

For a context that must deny the Grep *tool* too, the escape becomes human-gated
(`permissions.ask`) or a sanctioned audited `legion` command — never an agent-set env var.

## Why the plugin can't just ship it

Verified against CC v2.1.159: plugin manifests expose `skills`, `commands`, `agents`, `hooks`,
`mcpServers`, `lspServers`, `outputStyles`, `channels`, `userConfig` — **no `permissions`
field**, and plugin-shipped `settings.json` supports only `agent` / `subagentStatusLine`. So
the deny ruleset is documented here for operators to apply, not bundled. The plugin's
contribution is sym coverage (`sym def/refs/hover` + `sym list` #558), the Grep-tool guidance
hooks, and this doc.

## What changes in the plugin

- The `LEGION_BYPASS_GREP_HARD` hard-bypass is **removed** from `pre-bash-grep.sh` — it was the
  frictionless escape that made enforcement optional. The hook remains as soft fallback
  guidance for operators who have not yet set the deny rule (inject sym context, refuse the
  soft bypass on a local symbol hit), but it no longer advertises an always-allow escape.
- Fully retiring `pre-bash-grep.sh` (now that `permissions.deny` is the real mechanism) is a
  separate follow-up.

## The shape, stated plainly

1. **sym / sym list / recall** — the answer when one exists (indexed code, symbols, memory).
2. **Grep / Glob tool** — the sanctioned escape for non-indexed / free-text content; allowed,
   hooked, observable; auto-allows when sym can't serve.
3. **shell grep / rg / find** — denied by operator `permissions.deny`. The redundant,
   bypass-prone path. Gone.

A gate is only as good as the path it forces you onto. The mandatory gate is the harness's;
the plugin's job is to make the path it forces you onto — sym — actually serve the need.
