#!/bin/bash
# Legion PreToolUse Bash hook: block agents from touching the legion SQLite
# database directly.
#
# The legion CLI is the sole API for legion state. Any `sqlite3` / `sqlite`
# invocation OR any Bash command containing a reference to `legion.db` is a
# path to corruption: raw SQL bypasses migrations, violates constraints the
# Rust code enforces, and skips the audit log. Read-only peeks also tempt
# agents to "fix just this one row" and never learn to use the CLI.
#
# What the hook blocks:
#   - Any command whose executable is `sqlite` or `sqlite3`
#   - Any command whose argv contains the literal substring `legion.db`
#
# What passes:
#   - `legion <anything>` -- the rusqlite calls inside the binary are fine
#   - `sqlite3 /tmp/something-else.db` -- non-legion SQLite use is allowed
#   - File operations on paths that do not contain `legion.db`
#
# Bypass: if you genuinely need to run sqlite3 against legion.db (emergency
# recovery, forensic dump), do it outside Claude Code in your own terminal.
# There is no env-var override here -- the block is hard. Agents never have a
# legitimate reason to reach past the CLI.

INPUT=$(cat)

CMD=$(echo "$INPUT" | jq -r '.tool_input.command // empty' 2>/dev/null)

if [ -z "$CMD" ]; then
  exit 0
fi

# Case 1: references legion.db directly (covers sqlite3, cp, mv, rm, dd, etc.)
if echo "$CMD" | grep -qE 'legion\.db'; then
  jq -n --arg reason "Blocked: this command references \`legion.db\` directly. Use the \`legion\` CLI instead. Legion is the sole API for legion state -- direct DB access bypasses migrations, constraints, and the audit log. Commands like \`legion kanban view\`, \`legion recall\`, \`legion bullpen\`, \`legion reflect\`, and \`legion status\` expose structured fields through the binary. If you need something the CLI does not expose, file an issue for a new CLI command or JSON output flag -- do not reach past the binary." '
    {
      "hookSpecificOutput": {
        "hookEventName": "PreToolUse",
        "permissionDecision": "deny",
        "permissionDecisionReason": $reason
      }
    }
  '
  exit 0
fi

# Case 2: sqlite3 / sqlite invocation with ANY arguments (interactive mode
# could attach to legion.db after launch; blocking the binary entirely is
# the safer rule).
if echo "$CMD" | grep -qE '(^|[[:space:];|&(])sqlite3?([[:space:]]|$)'; then
  jq -n --arg reason "Blocked: agents do not run \`sqlite3\` / \`sqlite\` directly. Use \`legion\` subcommands for all legion state. The legion binary owns the DB and enforces schema, migrations, and audit invariants that raw SQL bypasses. If you need structured output from a legion command that does not exist yet, file a kanban card for the missing CLI feature instead of reaching past the binary." '
    {
      "hookSpecificOutput": {
        "hookEventName": "PreToolUse",
        "permissionDecision": "deny",
        "permissionDecisionReason": $reason
      }
    }
  '
  exit 0
fi

exit 0
