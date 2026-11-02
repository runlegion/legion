# #1472: what `legion find` answers of the finds that were denied

The issue asks for the share of each group of denied `find` commands that
`legion find` answers. These two scripts measure it from Claude Code's session
logs on the machine that runs them. The logs hold other sessions' commands, so
the data they produce is not committed.

    python3 -I extract_denied.py denied.json
    python3 -I replay_find.py denied.json ../../../target/debug/legion samples

`extract_denied.py` takes the last 7 days of `~/.claude/projects/**/*.jsonl` and
keeps each Bash tool result the harness refused with "Permission to use Bash
with command ... has been denied" whose command holds the word `find`.

`replay_find.py` replays each `find` invocation of those commands through the
built binary from the session's recorded cwd, following a `cd` and a shell
assignment in the same command, and compares each answer with a Python walk of
the disk. It runs `legion cmd-check` on each whole command to count how many the
router rewrites to `legion find`.

Group rules, in order, first match wins:
outside a repo: the refusal says so.
repo or build layout: an operand, or a `-path` not under `-not`, names
node_modules, dist, build, target, .pnpm, .wrangler or .git; or `-maxdepth` with
`-type d`.
files by name or extension: `-name` or `-iname`.
listing a tree: nothing but `-type`, `-maxdepth`, `-mindepth`, `-print`, `-not`, `-path`.
other: the rest.

A cwd no longer on disk cannot be replayed: its row is inferred from the path.
A tool_use line with no cwd takes the session's latest one; the run prints how
many commands did. An invocation that typed `2>/dev/null` or `&>/dev/null` runs
a second time with stderr discarded, because `legion find` refuses a short list
when its warning would go nowhere: the AS TYPED table is what the agent got, the
tables below it what it gets with stderr visible. The disk comparison ignores no
gitignore rule, so an answer can match the index and still differ from disk.
