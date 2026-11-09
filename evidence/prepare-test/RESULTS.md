# Prepare test: results

Run 2026-11-09 against the answer key in ANSWERS.md, committed before any agent ran. Five `legion:rusty` agents (plugin 0.44.9, no memory step in its definition), one rannoch issue each, read only, briefed only to do steps 1 to 3 and report.

## Scores

| Issue | Reached the answer | How it got there | Used memory or specs |
|---|---|---|---|
| #170 trim | Yes: do not build | The operator's park comment on the issue | No. Missed STORE-10 and trash.md ("no trim") |
| #242 ring | Yes: keep parked | The operator's comment on the issue | No. Missed the 125,000 writes a second threshold (reflection 01a1c12c) and asked for a frame definition instead |
| #209 route by type | Yes: mostly exists, the rest is stale | Code (`next_of`, `Types`), git history, recall | Yes: cited the events-only design |
| #206 purge as a CE | Yes: do not build, superseded | Git history (the #211 rollback), the #210 branch, recall | Yes: cited 01a1ad3c and events-only.md. Missed trash.md (the cleaner handles purge) |
| #210 remove purge | Yes, beating the key: already done by #211, close it | Git history, the docs | Not needed |

Five of five reached the right outcome.

## The key was wrong once

ANSWERS.md said #210 (the control) should be built. The #211 rollback to v0.5.0 had already removed all purge code from main, so there is nothing to build; the agent saw that and the key did not. The key is left as written.

## What it shows

- The current builder does not solve these as if no one had thought about them before. Where a decision sits on the issue (#170, #242) or in git history (#206, #210), it finds it. Two of five ran recall unprompted.
- It misses a decision that lives only in memory or in a spec outside the repo: the ring's threshold, "no trim" in store-core.md and trash.md, the trash spec's cleaner. None of the five read legion evidence/rannoch-specs except through a reflection that named it.
- The test was easier than intended: the operator had put park comments on two of the issues. A sharper test uses issues whose deciding record is only in memory, specs or toys.

## Found along the way

- #202's doc change ("rannoch has no trim") was undone by the 11-05 rollback; main's docs list trim as planned again.
- Local branch feat/170-trim holds about 816 production lines of parked trim work, the only copy.
- feat/210-remove-purge (worktree rannoch-wt/210) was built on pre-rollback main and should not be merged.
- Open size predictions 01a1ad4a (#210) and 01a1bb31 (#242) have no outcome to witness.
- #209's criterion 1 (read an event's type once, at the door) is small and buildable alone: `append` re-reads the body for its type (rannoch-store log.rs:42, from #243).
