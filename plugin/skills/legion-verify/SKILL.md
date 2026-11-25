---
name: legion-verify
description: |
  The final evidence gate before finished work is called done. Read the issue and the spec, learn
  what was supposed to be built, score the result of the work against the scenarios and the intent
  of the spec and issue, and signal drift. Emit one verdict per criterion -- pass, fail, or
  uncertain -- each with cited evidence (a test name, a file:line, an observable behavior).
  `legion verify --issue` records the verdicts as given and decides the work's fate: all-pass
  records a clean gate, any fail hard-blocks, any uncertain needs a human. For
  a solo engineering team this is the QA the operator does not have -- run it after review, before
  close.
version: 1.1.0
user-invocable: true
allowed-tools: Bash, Read, Write
---

# Legion Verify

The terminal gate. Build, PR-write, and review came before. Your job: read the issue and the
spec, learn what was supposed to be built, score the result of the work against the scenarios
and the intent of the spec and issue, and signal drift. You score criterion by criterion, with
evidence. It is
non-skippable: `legion issue close` refuses an issue that has acceptance criteria until verify
records a clean verdict.

You are auditing, not advocating. You wrote (or reviewed) this work; now read it as the QA
engineer the operator cannot hire. A criterion you cannot mechanically confirm is `uncertain`,
never `pass` -- verify never asserts an unprovable claim.

## Procedure

1. **Load the criteria.** They are the issue's, not your memory of them:

   ```bash
   legion issue view --repo <repo> --number <n>
   ```

   Every line under "Acceptance criteria" needs exactly one verdict. (If the issue declares a
   `## Traces to` requirement, judge against the referenced requirement's criteria -- the
   legion-verify AGENT covers that spec-conformance audit; this skill is the criterion-by-criterion
   verdict path.)

2. **Gather evidence.** For each criterion, find the proof:
   - `git diff main..HEAD` -- the change that addresses it.
   - Read CI's run on the verified head and note the test that exercises it. Run tests yourself only when CI has no run for that head, and then only the issue's own scenarios: a local full-suite run repeats CI.
   - For a behavioral criterion, describe the observable behavior you confirmed.
   - For the predictions criterion, fetch what was emitted for this issue by its issue ref --
     `legion uncertainty predictions --issue <owner>/<repo>#<n> --json` -- not by searching
     emission text, and witness each row it returns. When it returns zero rows, state plainly
     that no prediction carries that issue ref; zero rows is not "all witnessed". If stderr
     reports "most recent N of M", the rows were cut at `--limit`: re-run with `--limit M`
     or higher so every prediction for the issue is witnessed.

3. **Judge each criterion** honestly:
   - `pass` -- satisfied, and you can cite the test / file:line / behavior that proves it.
   - `fail` -- not satisfied, or the evidence contradicts it.
   - `uncertain` -- you cannot mechanically confirm it (e.g. a performance or UX claim with no
     test). Do not round up to pass.

   The judgment is yours: `legion verify` records each verdict as you give it and never demotes
   one by the wording of its evidence. When in doubt, mark the criterion `uncertain` and let a
   human adjudicate -- that is the gate working correctly, not a failure.

4. **Write the verdicts** to a JSON file (e.g. `/tmp/verdicts-<n>.json`) -- a list, one
   object per criterion. The shape depends on the issue's trace.

   Untraced issue (no `## Traces to` section, or `- None -- <reason>`) -- the issue shape:

   ```json
   [
     {"criterion": "<criterion text>", "verdict": "pass", "evidence": "tests::foo_does_x"},
     {"criterion": "<criterion text>", "verdict": "uncertain", "evidence": "perf claim, no benchmark"}
   ]
   ```

   Traced issue (`## Traces to` names a requirement) -- the spec shape, one entry per
   criterion the trace scopes (every criterion of the requirement, or only the ids in
   `[criteria: id, id]`):

   ```json
   [
     {"spec_doc_id": "<requirement id>", "spec_revision": 3, "criterion_id": "<criterion id>",
      "verdict": "pass",
      "artifacts": [{"kind": "test", "ref": "tests::foo_does_x", "outcome": "passed"}]}
   ]
   ```

   `spec_revision` is the requirement's current revision; a stale value is refused with the
   expected revision named. `kind` is `test`, `diff` or `command`; `artifacts` may be omitted.

   Provide one entry per criterion.

5. **Record the verdict:**

   ```bash
   legion verify --repo <repo> --issue <n> --verdicts-file /tmp/verdicts-<n>.json
   ```

   - All `pass` -> records a clean `legion-verify:issue-<repo>#<n>` gate; the
     issue's close gate is unblocked.
   - Any `fail` -> exits non-zero; close stays blocked. Finish the work and re-verify.
   - Any `uncertain` -> exits non-zero for a human to adjudicate; close stays blocked.
   - No criteria on the issue -> blocked: the issue was not SOLID. Add checkable criteria upstream.

## Notes

- Verify owns acceptance: does the work meet the criteria the issue -- or its traced spec --
  states. Whether this is the right work at all, whether that spec or issue should be
  different, is a separate question owned upstream (the design process for a spec, the
  operator for an issue), not by this gate.
- A criterion the diff satisfies is `pass` even if you would have designed it differently.
  Verify measures the work against its criteria as written -- the traced spec's when there is
  one, otherwise the issue's -- never against the design you would have chosen. If you believe
  the criterion itself is wrong, halt and escalate rather than failing work that conforms.
- The verdict is keyed on the issue (`legion-verify:issue-<repo>#<n>`), not the commit, so it
  survives the commit `legion issue close` runs on. Re-verify if the work materially changed after
  you recorded a verdict.
- Do not pad evidence to clear a verdict. "uncertain" for a human is the correct, honest outcome
  for an unprovable criterion -- that is the gate working, not failing.
