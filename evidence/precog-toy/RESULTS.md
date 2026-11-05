# Precog toy: results

Run 2026-11-05 on 439 tool calls from two orchestrator transcripts (this session and the one before it). The builder transcripts did not run: the toy failed on their file names, so builders are not measured yet. Memory is legion.db as it stood, through `legion recall --limit 3`.

## Against the predictions

| # | Prediction | Result | Verdict |
|---|---|---|---|
| 1 | Recall under 300 ms at the median, not under 50 | p50 85 ms, p95 120 ms; the context peek is 1.3 ms | Held |
| 2 | About 30% of moments have a top-3 already in context | 7% | Missed |
| 3 | About 40% have a top-3 not in context | 99%, but see below: most of it is the same three reflections | Missed, and the number means little |
| 4 | Recall returns the sym rule at no more than half the search moments after it was stored | 3 of 5 | Missed: better than predicted, on 5 moments |
| 5 | Where it was in context, it sat more than 50 turns back | 1,617 to 1,658 lines back; lines, not turns, so not comparable | Not tested as written |

## What it shows

**Both peeks are cheap.** About 85 ms to ask memory and about 1 ms to look through the context, so a peek at every tool call costs around 0.1 s.

**Asked with the task, memory answers with the same doctrine.** 372 different reflections came back across 439 moments, but three came back far more than the rest: two "autonomy posture" doctrine notes (140 and 94 times) and one about the auto-mode classifier (140 times). They are long, general, and match almost any working text. Injecting the top 3 at each moment would mostly repeat those three.

**The sym case is the important one.** At 3 of the 5 search moments after the sym rule was stored, recall did return it, and at all 3 it was already in the transcript, about 1,600 lines back. It was there and it was ignored. Having the memory in context did not change the action.

## What this means for precog

- Cost is not the problem.
- Being in context is not enough: what matters is where, and the sym rule was far back. "Already there" has to mean recent, not anywhere in the window.
- Retrieval keyed on the task is dominated by general doctrine. Precog needs to filter out reflections that match everything, and likely a second query keyed on the action (a search, an edit, a merge), which is what the sym rule is about.

## Limits

- Two transcripts, one of them this one. Builders are not in it yet.
- Five search moments for the sym case.
- "In context" is a match on the reflection's id or its first 60 characters, so a paraphrase does not count.
- Distance is in lines, not turns.
