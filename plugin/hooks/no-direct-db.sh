#!/bin/bash
# Legion PreToolUse Bash hook: block agents from touching the legion SQLite
# database directly.
#
# The legion CLI is the sole API for legion state. Any Bash command that
# names `legion.db` in its arguments -- sqlite3, cp, mv, rm, dd, cat, etc.
# -- is blocked. Raw access bypasses migrations, violates constraints the
# Rust code enforces, and skips the audit log. Read-only peeks also tempt
# agents to "fix just this one row" and never learn to use the CLI.
#
# What this hook blocks:
#   - Any command whose argv contains the literal substring `legion.db`
#
# What this hook does NOT block:
#   - `sqlite3 /tmp/other.db` / `sqlite3 $HOME/project/app.sqlite` -- other
#     projects legitimately use sqlite3 and agents should be able to work
#     with them
#   - `legion <anything>` -- the rusqlite calls inside the binary are fine
#   - File operations on paths that do not contain `legion.db`
#
# Known gap (accepted): an agent could run `sqlite3 /tmp/other.db` and then
# `ATTACH '.../legion.db' AS legion;` from the interactive prompt. This
# hook cannot see stdin commands. The mitigation is cultural, not technical
# -- agents are told to use the CLI, this hook catches the obvious misuse,
# and ATTACH would show up in shell history or an audit. Do not try to ban
# all sqlite3 usage to close this gap; the false-positive cost on other
# projects is too high.
#
# Bypass: if you need to run raw SQL against legion.db (emergency recovery,
# forensic dump), do it outside Claude Code in your own terminal. No env-var
# override.

# shellcheck source=lib/prelude.sh
source "${CLAUDE_PLUGIN_ROOT:-}/hooks/lib/prelude.sh" 2>/dev/null || exit 0
# shellcheck source=lib/emit.sh
source "${CLAUDE_PLUGIN_ROOT:-}/hooks/lib/emit.sh" 2>/dev/null || exit 0

legion_hook_parse || exit 0

CMD=$(legion_hook_field '.tool_input.command')

if [ -z "$CMD" ]; then
  exit 0
fi

# Skip enforcement in repos legion does not cover (#353).
legion_hook_covered || exit 0

if echo "$CMD" | grep -qE 'legion\.db'; then
  emit_deny "Blocked: this command references \`legion.db\` directly. Use the \`legion\` CLI instead. Legion is the sole API for legion state -- direct DB access bypasses migrations, constraints, and the audit log. Commands like \`legion kanban view\`, \`legion recall\`, \`legion bullpen\`, \`legion reflect\`, and \`legion status\` expose structured fields through the binary. If you need something the CLI does not expose, file an issue for a new CLI command or JSON output flag -- do not reach past the binary."
  exit 0
fi

exit 0
