---
name: legion-pr-write
description: |
  Compose the PR body as a forcing function before opening a PR. Map each of the issue's
  acceptance criteria to the change that satisfies it -- in your own prose, with evidence --
  and state what you deliberately did NOT do. Articulation is verification: writing the
  mapping makes you re-read your own diff as a reader and catch what you talked past while
  coding. Validate the body with `legion pr write-check`, which refuses an empty or
  boilerplate mapping and records the gate `legion pr create` requires. Run this after
  `/legion-simplify`, before review.
version: 1.0.0
user-invocable: true
allowed-tools: Bash, Read, Write
---

# Legion PR-Write

The step between simplify and review. You write the PR body that maps the issue's
acceptance criteria to your change, then validate it. The body you produce is the claim
set the verify gate (#520) and the human reviewer read -- so write it for a reader who
has not seen your work.

`legion pr create` will not open the PR until this gate is clean on HEAD (alongside the
simplify gate).

## Procedure

1. **Load the acceptance criteria.** You are mapping against the issue, not your memory of it:

   ```bash
   legion issue view --repo <repo> --number <issue>
   ```

   Note every item under "Acceptance criteria". You will write one mapping entry per item.

2. **Re-read your own diff.** `git diff main..HEAD`. For each acceptance criterion, find the
   hunk that satisfies it. If you cannot point to one, the criterion is not met -- stop and
   finish the work (or move it to "Not done" with a reason).

3. **Write the body to a file** (e.g. `/tmp/pr-body-<issue>.md`) with exactly these sections:

   ```markdown
   ## Summary

   One paragraph: what the change does and why.

   ## Acceptance criteria mapping

   ### 1. <criterion, paraphrased>
   Prose: which change satisfies this criterion and how. Explain the mechanism -- do not
   restate the criterion. This is the forcing function; write it as an explanation.
   Evidence: <a test name, a file:line, or an observable behavior>

   ### 2. <next criterion>
   ...

   ## Not done

   What you deliberately left out of scope, and why. Write "nothing" only if that is true.
   ```

   One `### ` entry per acceptance criterion. Each entry needs real explanatory prose and a
   line of evidence. The mapping must be composed prose, not a checklist -- a fill-in form
   defeats the purpose.

   The body also needs a GitHub closing keyword (`Closes #<issue>`) so the merge auto-closes
   the issue -- see step 5, `--closes` appends it for you.

4. **Validate.** This records the gate and refuses boilerplate:

   ```bash
   legion pr write-check --repo <repo> --issue <issue> --body-file /tmp/pr-body-<issue>.md
   ```

   - Clean -> the gate is recorded; proceed to open the PR.
   - Gaps reported -> fix the body (each gap names the entry and what is missing) and re-run.
     Do not pad to clear the word count; write the real explanation. If a criterion genuinely
     is not addressed, that is a signal the work is not done.

5. **Open the PR** with the validated body, passing `--closes` for the issue it satisfies so
   the merge auto-closes it (#751; repeatable for more than one issue, quote the cross-repo
   form since `#` starts a shell comment):

   ```bash
   legion pr create --repo <repo> --title "<type>(#<issue>): <summary>" \
     --body "$(cat /tmp/pr-body-<issue>.md)" --task <card-id> --closes <issue>
   # cross-repo: --closes 'owner/repo#<issue>'
   ```

## Notes

- The validator checks structure, not prose quality: one substantive entry per criterion,
  each citing evidence, plus a non-empty "Not done" section. It cannot tell a good
  explanation from a mediocre one -- that is your job and the reviewer's. It can tell when
  you pasted the criteria back, and it will refuse.
- If you commit again after a clean gate, the gate no longer matches HEAD -- re-run step 4.
- Re-running write-check overwrites the previous gate row for HEAD; the most recent wins.
- Bootstrap only: a branch that ships these skills themselves cannot gate on them. Use
  `legion pr create --skip-gates` (it writes an audit row so the skip is never silent).
- `legion pr write-check` warns (does not fail, v1) when the validated body has no closing
  keyword for `--issue`. `legion pr create --closes <issue>` appends `Closes #<issue>` (or
  `Closes owner/repo#<issue>` cross-repo) itself unless one is already present -- idempotent,
  so re-running it never duplicates the line.
- GitHub requires the closing keyword to immediately precede EACH reference -- `Closes #1,
  #2` closes only #1, not #2 (`Closes #1, closes #2` closes both). Repeatable `--closes`
  handles this correctly (one `Closes #N` line per issue); if you hand-write a body instead,
  repeat the keyword per issue rather than comma-joining bare references, or run
  `legion kanban reconcile` after merge to catch anything that silently stayed open.
