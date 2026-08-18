---
name: legion-verify
description: |
  The final evidence gate before finished work is called done. Read the issue's acceptance
  criteria, the diff, and the test output, and emit one verdict per criterion -- pass, fail, or
  uncertain -- each with cited evidence (a test name, a file:line, an observable behavior).
  `legion verify --issue` records the verdict and decides the work's fate: all-pass records a
  clean gate, any fail hard-blocks, any uncertain (or a pass with no evidence) needs a human. For
  a solo engineering team this is the QA the operator does not have -- run it after review, before
  close.
version: 1.1.0
user-invocable: true
allowed-tools: Bash, Read, Write
---

# Legion Verify

The terminal gate. Build, simplify, PR-write, and review came before; verify confirms the work
actually satisfies what the issue asked for, criterion by criterion, with evidence. It is
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
   - Run the tests (`cargo test`, the relevant suite) and note the test that exercises it.
   - For a behavioral criterion, describe the observable behavior you confirmed.

3. **Judge each criterion** honestly:
   - `pass` -- satisfied, and you can cite the test / file:line / behavior that proves it.
   - `fail` -- not satisfied, or the evidence contradicts it.
   - `uncertain` -- you cannot mechanically confirm it (e.g. a performance or UX claim with no
     test). Do not round up to pass.

   **Vacuous evidence is not evidence.** The gate mechanically rejects two patterns as vacuous
   (demoting `pass` to `uncertain` automatically), and you must reject them in your judgment too
   when the heuristic cannot decide:

   - *Restatement*: the evidence text is a copy or near-copy of the criterion. Example: criterion
     "returns error on empty input", evidence "returns error on empty input" -- that is not proof,
     it is repetition. Cite the test that confirmed it instead.
   - *No assertion marker*: the evidence describes only what the code does, not what was confirmed.
     Example: "added match arm for empty case" names an implementation detail, not a verification.
     Instead, cite a test name (`tests::empty_input_returns_error`), a file:line (`src/lib.rs:42`),
     or an observed outcome ("running `legion verify` with no AC exits 1 and prints NoCheckableAc").

   The mechanical heuristic is a floor, not a ceiling. Where it cannot decide, you judge. When in
   doubt, mark the criterion `uncertain` and let a human adjudicate -- that is the gate working
   correctly, not a failure.

4. **Write the verdicts** to a JSON file (e.g. `/tmp/verdicts-<n>.json`) -- a list, one
   object per criterion:

   ```json
   [
     {"criterion": "<criterion text>", "verdict": "pass", "evidence": "tests::foo_does_x"},
     {"criterion": "<criterion text>", "verdict": "uncertain", "evidence": "perf claim, no benchmark"}
   ]
   ```

   Provide one entry per criterion. A `pass` with empty or vacuous evidence is treated as
   `uncertain` (see "Judge each criterion" above for what counts as vacuous).

5. **Record the verdict:**

   ```bash
   legion verify --repo <repo> --issue <n> --verdicts-file /tmp/verdicts-<n>.json
   ```

   - All `pass` (with evidence) -> records a clean `legion-verify:issue-<repo>#<n>` gate; the
     issue's close gate is unblocked.
   - Any `fail` -> exits non-zero; close stays blocked. Finish the work and re-verify.
   - Any `uncertain` -> exits non-zero for a human to adjudicate; close stays blocked.
   - No criteria on the issue -> blocked: the issue was not SOLID. Add checkable criteria upstream.

## Notes

- Verify owns acceptance (does the work meet the stated criteria). Vault owns intent (is this
  the right work / is the spec serviced). They are separate gates; do not conflate them.
- The verdict is keyed on the issue (`legion-verify:issue-<repo>#<n>`), not the commit, so it
  survives the commit `legion issue close` runs on. Re-verify if the work materially changed after
  you recorded a verdict.
- Do not pad evidence to clear a verdict. "uncertain" for a human is the correct, honest outcome
  for an unprovable criterion -- that is the gate working, not failing.
