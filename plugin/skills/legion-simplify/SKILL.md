---
name: legion-simplify
description: |
  Review the current branch diff for code quality issues: duplicate logic, unnecessary
  abstraction, stringly-typed state, and other structural problems. Articulate, per changed
  file, what you checked and why the verdict holds; `legion quality-gate check` validates the
  articulation (coverage + non-boilerplate) and records the gate so `legion pr create` can
  verify clean state before opening a PR. Run this on every feature branch before a PR.
version: 2.0.0
user-invocable: true
allowed-tools: Bash, Read
---

# Legion Simplify

Review the git diff on this branch for structural quality issues, and write down -- per
changed file -- what you checked and the reasoning behind your verdict. `legion quality-gate
check` refuses the gate unless every changed file is addressed with real prose, then records
it so `legion pr create` can gate on it.

This is a forcing function, not a checkbox. The old version of this skill let you record
`"clean"` without reading the diff, and the corpus proved it: simplify caught structural
issues 0.7% of the time while `pr write-check` -- the same self-review, but with validated
prose -- caught 24%. You cannot clear this gate by typing "clean". You clear it by showing,
file by file, the reasoning that says clean.

## What to Review

Inspect `git diff main...HEAD` (or `git diff origin/main...HEAD`). Use the three-dot form
(`...`) -- it diffs against the merge base, so files changed only on main since your branch
point do not appear in your coverage list. The validator uses the same three-dot range. For each changed file, look for:

- **Duplicate logic** -- same transformation or check written twice; extract a shared helper
- **Unnecessary abstraction** -- a wrapper or trait that adds indirection without buying
  extensibility; flatten it
- **Stringly-typed state** -- `status: String` where `"pending" | "done"` would be an enum
- **Hand-rolled helpers** -- functions duplicating something already in `std` or a module helper
- **Unnecessary new parameters** -- a parameter that is the same value at every call site
- **Copy-paste variations** -- two near-identical blocks differing by one variable; loop or parametrize
- **Error swallowing** -- `.unwrap_or(())` or `let _ =` on a result that should propagate
- **Change-narrating comments** -- comments saying what the diff did rather than why the code exists

Do NOT flag: style preferences (naming, ordering) unless they violate project conventions;
pre-existing issues outside the diff; hypothetical future improvements.

## Output: the articulation file

Write a markdown file with one `### <path>` entry per changed file in the diff. Every changed
file must have an entry -- the validator refuses on any gap. Each entry is composed prose, not
a checklist: name the categories you actually checked against this file's hunks and give the
reasoning for the verdict. Entries under ~12 words are rejected as boilerplate, so a bare
"clean" or a restated category list will not pass. A `clean` verdict still requires the
reasoning -- "I read X, the duplicated-looking Y is justified because Z." A finding states the
problem, the file:line, and the fix.

```markdown
### src/db/foo.rs
Checked for duplicate logic and stringly-typed state across the two new query methods. The
WHERE-clause construction is identical in both (lines 40 and 92) -- duplicate logic, extractable
to a helper. State uses the GateResult enum, not strings -- clean there. No new abstraction.
Verdict: finding (duplication, MED).

### src/cli/bar.rs
Two table-printer fns look copy-pasted but diverge in columns and alignment; merging them would
need a generic table abstraction that costs more than the duplication saves -- intentionally left.
Error paths propagate via `?`. No stringly-typed state. Verdict: clean.
```

## Procedure

1. `git -c core.quotePath=false diff --name-only main...HEAD` (fall back to
   `origin/main...HEAD`) -- this is your coverage list. The `-c core.quotePath=false` flag
   ensures paths with non-ASCII characters are returned unescaped, matching the headings you
   write in step 2. Use `### <path>` headings that are repo-root-relative paths exactly as
   git reports them (e.g. `### src/café.rs`, not an escaped or shortened form).
2. For EACH changed file: read it, review its hunks against the categories above, and write its
   `### <path>` entry with real reasoning.
3. Write the articulation to a file (e.g. `/tmp/simplify-<branch>.md`).
4. Validate and record:
   ```bash
   legion quality-gate check \
     --skill legion-simplify \
     --result <clean|issues> \
     --findings-count <N> \
     --articulation-file /tmp/simplify-<branch>.md
   ```
   `--findings-count` is the number of real structural findings you recorded (0 for a clean
   verdict). The validator resolves the changed-file set from git, checks every file has a
   non-boilerplate entry, then records the gate for HEAD.
   - Clean -> the gate is recorded; proceed to pr-write.
   - Refused -> the output names the gap (an unaddressed file, a boilerplate entry). Fix it by
     actually reading that file and writing real reasoning -- do not pad to clear the check. Re-run.
5. If your verdict is `issues`, fix the findings, then re-run from step 1 on the new HEAD (the
   gate is HEAD-keyed) until the articulation is clean.

## Pass Criteria for `legion pr create`

`legion pr create` checks the DB before opening the PR. The gate passes only when a gate row
exists for the current HEAD commit hash with `result = "clean"`. Because the gate is recorded
only by `legion quality-gate check`, a clean row now means an accepted per-file articulation
exists -- not merely that someone typed "clean".
