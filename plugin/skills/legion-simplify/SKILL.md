---
name: legion-simplify
description: |
  Review the current branch diff for code quality issues: duplicate logic, unnecessary
  abstraction, stringly-typed state, and other structural problems. Articulate, per changed
  file, what you checked and why the verdict holds; `legion quality-gate check` validates the
  articulation (coverage + non-boilerplate + located evidence) and records the gate so `legion
  pr create` can verify clean state before opening a PR. Run this on every feature branch before a PR.
version: 2.1.1
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

Every entry -- clean or finding -- must cite a **within-file locator** in its body: a `file:line`
(e.g. `foo.rs:42`), a bare line reference (`line 42`, `lines 40 and 92`), a symbol (`fn name` /
`Type::method`), or an `Evidence:` line. Neither the `### <path>` heading nor a bare filename in
prose counts -- both just restate the file name. Point at the specific line or symbol you actually
read. An entry whose prose is substantive but cites nothing locatable is refused: "I checked it,
looks clean" with no anchor is precisely the rubber-stamp this gate stops.

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
   git reports them (e.g. `### src/café.rs`, not an escaped or shortened form) -- no `./`
   prefix, no trailing text after the path on the heading line. Coverage matching is exact
   string equality against what git printed; a heading that does not match byte-for-byte is
   refused as missing, no matter how much prose you add under it.

   **Renames**: a rename git still recognizes as such (similarity above its threshold --
   the common case, even with a real edit alongside the move) reports as one `R` line and
   `--name-only` prints just the new path, so it needs only the one new-path entry. Only a
   rewrite heavy enough to drop below git's similarity threshold splits into two separate
   paths -- the old path (deleted) and the new path (added) -- and both then need their own
   `### <path>` entry, including the old one, even though it no longer exists in the tree.
   That old-path entry still has to clear the same substance and locator bars as any other:
   a short paragraph noting where the content moved, with an `Evidence:` line pointing at the
   new path, clears both; a bare "moved to new_name.rs" does not.
2. For EACH changed file: read it, review its hunks against the categories above, and write its
   `### <path>` entry with real reasoning.
3. Write the articulation to a file (e.g. `/tmp/simplify-<branch>.md`).
4. If your verdict has real findings (`--result issues`), also write them as structured JSON --
   one object per finding, `{"file": "<path>", "line": <int or null>, "severity": "HIGH"|"MED"|"LOW",
   "summary": "<one-line problem statement>"}`. This is separate from the prose articulation: the
   articulation is for a human/reviewer to read, the structured array is what the finding-resolution
   ledger (#773) tracks toward resolved-or-dispositioned. The prose is NOT parsed for findings --
   only this array is.
5. Validate and record:
   ```bash
   legion quality-gate check \
     --skill legion-simplify \
     --result <clean|issues> \
     --findings-count <N> \
     --articulation-file /tmp/simplify-<branch>.md \
     --findings-json '[{"file": "src/foo.rs", "line": 42, "severity": "MED", "summary": "duplicate WHERE-clause construction"}]'
   ```
   `--findings-json` is optional and MUST be omitted (or empty) for a `--result clean` call --
   passing findings alongside `--result clean` in the same call is refused (#773, see below), not
   silently accepted. `--findings-count` is the number of real structural findings you recorded
   (0 for a clean verdict). The validator resolves the changed-file set from git, checks every
   file has a non-boilerplate entry, then records the gate for HEAD.
   - Clean -> the gate is recorded; proceed to pr-write. **Refused instead** when: this same call's
     `--findings-json` is non-empty (a clean verdict cannot carry its own findings), OR a HIGH/MED
     finding from a PRIOR run on this branch is still PENDING (neither a later commit touched its
     file nor was it explicitly dispositioned), OR a prior LOW finding is still un-acked (#773) --
     see "Finding resolution" below.
   - Refused (coverage/substance) -> the output names the gap (an unaddressed file, a boilerplate
     entry). Fix it by actually reading that file and writing real reasoning -- do not pad to clear
     the check. Re-run.
6. If your verdict is `issues`, fix the findings, then re-run from step 1 on the new HEAD (the
   gate is HEAD-keyed) until the articulation is clean.

## Finding resolution (#773)

A finding you record via `--findings-json` does not evaporate once the gate is recorded. It lives
in the findings ledger as PENDING until one of:

- **Resolved automatically** -- a later commit on this branch touches the flagged file. Detected
  by git log at the next `quality-gate check`/`record` call; you do not need to do anything extra
  beyond committing the fix. **Coarse by design:** this is file-level, not content-aware -- ANY
  commit that touches the flagged file resolves the finding, even an unrelated edit elsewhere in
  it, not only one that fixes the specific problem. On an active branch a file gets re-touched for
  many reasons, so a finding can auto-resolve without ever being deliberately addressed. If that
  matters for a specific finding, disposition or batch-ack it explicitly instead of relying on
  auto-resolution to be a stand-in for "someone looked at this."
- **Dispositioned** -- you decide not to fix it and say why:
  `legion quality-gate finding-disposition --id <finding-id> --reason "won't fix: intentional, see X"`.
- **Batch-acked** -- for a sweep of LOW/cosmetic findings only, one shared reason clears all of
  them at once (no per-item ceremony):
  `legion quality-gate finding-ack --branch <branch> --skill legion-simplify --reason "formatting only, deferred"`.

Find finding ids with `legion quality-gate finding-list --branch <branch> --skill legion-simplify`.
A `clean` verdict is refused until the PENDING set for this branch+skill is empty AND this call's
own `--findings-json` is empty -- writing a finding down and then recording clean on a LATER commit
without resolving or dispositioning it is one hand-wave this closes; recording clean in the SAME
call you report the finding in is the other, and is refused just as hard.

## Pass Criteria for `legion pr create`

`legion pr create` checks the DB before opening the PR. The gate passes only when a gate row
exists for the current HEAD commit hash with `result = "clean"`. Because the gate is recorded
only by `legion quality-gate check`, a clean row now means an accepted per-file articulation
exists -- not merely that someone typed "clean" -- AND that no prior finding on this branch is
still sitting unresolved and undispositioned (#773).

## Notes

- The validator checks structure, not reasoning quality: file coverage, a word count per
  entry (~12 words after the heading is stripped), and a within-file locator (`file:line`,
  `line N`, a symbol, or an `Evidence:` line). It cannot tell a correct verdict from a
  plausible-sounding wrong one -- that stays the reviewer's job, same division of labor as
  `legion pr write-check`. It can tell when an entry is empty, boilerplate, or points at
  nothing locatable, and it refuses those.
- If you commit again after a clean gate, the gate no longer matches HEAD -- re-run step 5.
