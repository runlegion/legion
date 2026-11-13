# Test-rule toy: results

Run 2026-11-13. Platform's spec-writer toy (research `01a1d1e9-dac6-7051-9ac9-22be7e6b027d`) on its own inputs: four mail requirements (FR-MAIL-049, 004, 050, 034), each run cold with `claude -p`. Predictions in PREDICTIONS.md, committed before each round.

| | 0.44.12 run (`out-0.44.12/`) | Round 1, revised rule (`out-revised/`) | Round 2, three lines only (`out-simple/`) |
|---|---|---|---|
| FR-MAIL-049 | 4 | 3 (1 behaviour, 2 unit) | 2 (1, 1) |
| FR-MAIL-004 | 2 | 2 (0, 2) | 9 (1, 8) |
| FR-MAIL-050 | 7 | 9 (3, 6) | 8 (3, 5) |
| FR-MAIL-034 | 12 | 9 (3, 6) | 11 (3, 8) |
| Total | 25 | 23 | 30 |

## Round 1: the revised rule (`prompt.md`)

1. Size 18 to 28: **holds**, 23.
2. Only two kinds: **holds**, behaviour and unit only.
3. Scenarios are complete behaviours: **holds**, 7 scenarios of 7 to 11 steps each.
4. Nothing unguarded: **holds**, 83 failure ways listed, every one points at a check or a gap.
5. The settled-outcome rule fires on FR-MAIL-049: **holds**, "a session whose registration with its mailbox fails is not logged in" is a check.
6. FR-MAIL-034 keeps its more-than-100 check: **holds**, "held returns expunged rows, more than 100 at a time".

## Round 2: only the three lines (`prompt-simple.md`)

7. Size 15 to 30: **holds**, 30, at the top.
8. Only two kinds: **holds**.
9. Scenarios are complete behaviours: **holds**, 8 scenarios of 4 to 13 steps each.
10. Nothing unguarded: **holds** for what it listed, 60 failure ways, every one caught or a gap.
11. The two hard cases: **refuted**. FR-MAIL-049 has no check that a session whose registration fails is not logged in. FR-MAIL-034 has no check for more than 100 expunged rows.

Round 2 also split FR-MAIL-004's address examples into 8 unit checks where round 1 kept one golden table: without "examples are rows, not checks", examples became checks again.

## What it means

The three lines hold for the shape: two kinds, scenarios that are complete behaviours, and every failure way it found accounted for. What they lose is the writer's craft, not the shape: the settled outcome needs its worked example, a failure someone saw needs "keeps its check", and many examples need "examples are rows". Those already live in the spec-writer (0.44.12, and #1527), not in the team's rule. So the team's rule can stay three lines, and the spec-writer keeps the rules that make a writer apply them well.

## Rerun

`bash run.sh` (round 1, `prompt.md`) and `bash run-simple.sh` (round 2, `prompt-simple.md`) each write `out/`; move it aside between runs. Inputs are in `in/`.
