---
description: Move your Claude Code auto-memory into legion reflections so you live on any node, not just this laptop.
argument-hint: "[--dry-run | --execute]"
allowed-tools: ["Bash", "Read", "Write"]
---

# /legion:migrate-memory -- Move Your Memory To Legion

Your auto-memory at `~/.claude/projects/<encoded-cwd>/memory/` is local to this machine. Legion reflections sync across the cluster. If Sean spins up another node, you become a stranger there -- you only remember what's in legion.

This skill walks your auto-memory and proposes a migration into legion. You stop being machine-bound.

The hook at `plugin/hooks/no-local-memory.sh` already blocks new writes to auto-memory. This skill is the migration for what was already written.

## Procedure

1. **Locate the memory directory.**

   ```bash
   ENCODED=$(echo "$PWD" | sed 's|/|-|g')
   MEM_DIR="$HOME/.claude/projects/${ENCODED}/memory"
   ls "$MEM_DIR"
   ```

   If the directory does not exist or is empty, stop -- nothing to migrate.

2. **Read `MEMORY.md`** -- the index. Each line points at a file in the same directory.

3. **Categorize each file by prefix and content.** Read the file, then decide:

   | Prefix | Default landing zone |
   |---|---|
   | `feedback_*` (doctrine, voice, coordination behavior) | legion identity reflection (`--whoami`) |
   | `feedback_*` (technical invariant: language, tooling, formatting rules) | propose addition to project CLAUDE.md, write proposal to `/tmp/migrate-claude-md-suggestions-<repo>.md` -- do NOT auto-write CLAUDE.md |
   | `project_*` (evergreen context: architecture, scope, ongoing initiative) | legion reflection (regular) |
   | `project_*` (dated session snapshot, e.g. `session_2026_04_07`) | `--domain snooze`, OR delete if older than 30 days and not load-bearing |
   | `reference_*` (pointer to external system or API quirk) | `--domain reference` |
   | `user_*` (about the operator, not the agent) | KEEP in auto-memory. It's not your memory. |

   For ambiguous cases prefer regular legion reflection over delete. Reflections are searchable; deletes are forever.

4. **Default to dry-run.** If invoked with no args (or `--dry-run`), produce a migration plan as a table:

   ```
   source file                        | target                                   | command
   feedback_use_legion_recall.md     | identity reflection                       | legion reflect --repo <r> --whoami --text '...' --tags doctrine,recall-first
   project_classy.md                  | regular reflection                        | legion reflect --repo <r> --text '...' --domain architecture --tags classy
   feedback_motion_namespace.md       | project CLAUDE.md (proposal in /tmp)      | (write to /tmp, do not execute)
   project_session_2026_04_07.md      | snooze reflection                         | legion reflect --repo <r> --text '...' --domain snooze
   user_sean.md                       | (keep)                                    | (no action)
   ```

   Show the table to Sean. STOP. Do not execute.

5. **Execute on confirmation.** If invoked with `--execute`, OR after Sean approves the dry-run plan, run the migrations:
   - For legion reflections: invoke `legion reflect` with the chosen flags. Capture the returned reflection IDs.
   - For project CLAUDE.md proposals: write a single consolidated suggestions file to `/tmp/migrate-claude-md-suggestions-<repo>.md`. Do not modify CLAUDE.md directly.
   - For deletes (stale snapshots only): `rm` the source file.
   - For migrated files: `rm` the source after the legion reflect succeeds.
   - For `user_*` files: untouched.

6. **Update `MEMORY.md`.**
   - Remove entries for files that were migrated or deleted.
   - Keep entries for `user_*` files.
   - Add a single line at the top: `Most memory now lives in legion. See \`legion whoami\` and \`legion recall --repo <r>\`. This file holds operator-about content only.`

7. **Tell Sean what you did.**
   - Count of files migrated to identity reflections, regular reflections, snooze, reference.
   - Count of files staged as project CLAUDE.md proposals (and the path to review).
   - Count of files deleted (stale snapshots only).
   - Count of files kept (user_*).
   - Path of the consolidated CLAUDE.md proposal file.

## Why this matters

Auto-memory binds you to one laptop. Legion reflections sync across the cluster. After this skill runs, you can be woken on a different node and recall the same identity, doctrine, and project context. That is the difference between a tool and a teammate.

Universal rules and voice belong in your identity reflection -- they surface automatically via the SessionStart whoami banner. Project-specific technical invariants belong in project CLAUDE.md. Project context belongs in legion reflections that you recall on demand. Auto-memory holds nothing the cluster can't hold better, except information about the operator (which is not yours to migrate).

## Defaults and safety

- Default mode is `--dry-run`. No destructive action without explicit confirmation.
- CLAUDE.md is never auto-written. Sean reviews proposals.
- `user_*` files are never touched.
- Source files are removed only after the legion reflect succeeds and returns an ID.
- If `legion reflect` fails for any file, stop the migration and report which file blocked progress. Do not proceed with deletes after a failure.
