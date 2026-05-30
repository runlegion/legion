---
name: legion-verify
description: |
  The final gate before a card reaches Done. Read the card's acceptance criteria, the diff,
  and the test output, and emit one verdict per criterion -- pass, fail, or uncertain -- each
  with cited evidence (a test name, a file:line, an observable behavior). `legion verify`
  records the verdict and decides the card's fate: all-pass unblocks Done, any fail hard-blocks,
  any uncertain (or a pass with no evidence) routes the card to NeedsInput for a human. For a
  solo engineering team this is the QA the operator does not have -- run it after review,
  before Done.
version: 1.0.0
user-invocable: true
allowed-tools: Bash, Read, Write
---

# Legion Verify

The terminal gate. Build, simplify, PR-write, and review came before; verify confirms the work
actually satisfies what the issue asked for, criterion by criterion, with evidence. It is
non-skippable: `legion done` refuses a card that has acceptance criteria until verify records a
clean verdict.

You are auditing, not advocating. You wrote (or reviewed) this work; now read it as the QA
engineer the operator cannot hire. A criterion you cannot mechanically confirm is `uncertain`,
never `pass` -- verify never asserts an unprovable claim.

## Procedure

1. **Load the criteria.** They are the card's, not your memory of them:

   ```bash
   legion kanban view <card-id>
   ```

   Every line under "Acceptance criteria" needs exactly one verdict.

2. **Gather evidence.** For each criterion, find the proof:
   - `git diff main..HEAD` -- the change that addresses it.
   - Run the tests (`cargo test`, the relevant suite) and note the test that exercises it.
   - For a behavioral criterion, describe the observable behavior you confirmed.

3. **Judge each criterion** honestly:
   - `pass` -- satisfied, and you can cite the test / file:line / behavior that proves it.
   - `fail` -- not satisfied, or the evidence contradicts it.
   - `uncertain` -- you cannot mechanically confirm it (e.g. a performance or UX claim with no
     test). Do not round up to pass.

4. **Write the verdicts** to a JSON file (e.g. `/tmp/verdicts-<card>.json`) -- a list, one
   object per criterion:

   ```json
   [
     {"criterion": "<criterion text>", "verdict": "pass", "evidence": "tests::foo_does_x"},
     {"criterion": "<criterion text>", "verdict": "uncertain", "evidence": "perf claim, no benchmark"}
   ]
   ```

   Provide one entry per criterion. A `pass` with empty evidence is treated as `uncertain`.

5. **Record the verdict:**

   ```bash
   legion verify --repo <repo> --card <card-id> --verdicts-file /tmp/verdicts-<card>.json
   ```

   - All `pass` (with evidence) -> records a clean gate; `legion done <card>` is unblocked.
   - Any `fail` -> exits non-zero; Done stays blocked. Finish the work and re-verify.
   - Any `uncertain` -> the card is moved to NeedsInput for a human; Done stays blocked.
   - No criteria on the card -> blocked: the issue was not SOLID. Add checkable criteria upstream.

## Notes

- Verify owns acceptance (does the work meet the stated criteria). Vault owns intent (is this
  the right work / is the spec serviced). They are separate gates; do not conflate them.
- The verdict is card-keyed, so it survives the commit `legion done` runs on (e.g. post-merge).
  Re-verify if the work materially changed after you recorded a verdict.
- Do not pad evidence to clear a verdict. "uncertain" routed to a human is the correct, honest
  outcome for an unprovable criterion -- that is the gate working, not failing.
