# Test-rule toy: predictions

Written 2026-11-13, before the run. Platform's spec-writer toy (research `01a1d1e9-dac6-7051-9ac9-22be7e6b027d`), re-run on its own inputs (four mail requirements: FR-MAIL-049, 004, 050, 034) with `prompt.md` updated to the revised testing rule from RFC `01a1d346` and its revisions: two kinds only (behaviour, unit); a scenario is one complete behaviour a user does, start to finish; a small check of one rule is a unit check; a refused attempt is a scenario of its feature; a never-property is a unit check; a real-run number is a PR note, never a check; the settled-outcome rule carries its worked example. `prompt-0.44.12.md` and `out-0.44.12/` are the previous run for comparison (25 checks; the research had 26 for 73 criteria).

The question: does the revised rule hold on real requirements, with nothing a SHALL needs left unguarded?

1. **Size.** Total checks across the four requirements land between 18 and 28. Confidence 0.65.
2. **Only two kinds.** No check is e2e, event or a measurement. Confidence 0.85.
3. **Scenarios are complete behaviours.** Judged per behaviour scenario: each is a sequence a user runs through a surface, not a single call or one small rule. Zero tiny scenarios. Confidence 0.6.
4. **Nothing unguarded.** Every failure way listed points at a check, or says why no SHALL breaks; no SHALL clause in a statement is left without a check. Confidence 0.7.
5. **The settled-outcome rule fires.** FR-MAIL-049's register-at-login outcome is a check, not a gap. Confidence 0.7.
6. **The seen defect stays guarded.** FR-MAIL-034 keeps its check for more than 100 messages. Confidence 0.8.
