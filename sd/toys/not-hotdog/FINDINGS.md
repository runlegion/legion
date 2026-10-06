# not-hotdog toy: findings (2026-10-06)

not-hotdog is step 1 of the double diamond: it works with the operator on an intent and keeps it to the
why. Definition: `not-hotdog.md`. Runs were isolated `claude -p` sessions with plugins and hooks off
(one global "identity missing" nudge still loaded; it says nothing about why or how).

## Test 1: classify statements (`run.py`, `score.py`, `gold.jsonl`)

36 real statements: revision 1 of the legion 0.5 intent (13 proposals, what_it_is, becoming; Sean
rejected it as details), revision 6 (accepted), the "same stream" line (removed after Sean's
correction), and 10 things Sean said in the design conversation (labels by prime).

| run | verdicts matching gold | gold why called not-why | gold not-why/mixed called why | mechanism words left in what it kept |
|---|---|---|---|---|
| opus a | 32/36 | 0 | 0 | 0 |
| opus b | 31/36 | 1 ("One memory, not many kinds.") | 0 | 0 |
| sonnet a | 33/36 | 0 | 0 | 1 ("central server") |

Agreement: opus a vs b 33/36; opus vs sonnet 30-31/36. Nearly every miss is not-why vs mixed, which
behave the same (need kept, mechanism parked). No run passed a how off as why.

The rewrites are needs, not mechanisms: "New capability listens to the same stream" became "As the
service grows, it stays one thing; nothing new becomes a separate place someone has to learn or check."

## Finding: classifying is not enough

Revision 1 through test 1 came out as 15 detailed needs, against revision 6's four directions. Every
mechanism was gone, but it was still details: "trying the same change twice never does harm" and
"wrong connections are caught" are requirements in why clothing, and guesses about the customer that
the workshop exists to find. Stating them plants the answer.

## Test 2: compose the whole intent (`compose.py`)

Added a compose pass: what_it_is (1-3 sentences), becoming (1-2), at most five directions; every other
need is either folded in, parked "for the workshop to find, not for the intent to state", or parked for
the spec agents. Run on revision 1 alone.

All three runs turned the rejected revision 1 into something shaped like the accepted revision 6:
memory at the moment of choice, finding out whether it helped, an honest record with operators asked
only for judgment, agents not doing the same work unknowingly, staying small enough to understand.
Opus parked 7-9 needs for the workshop and 19 notes for the spec agents. Sonnet let one mechanism into a
direction ("through an archivist each operator names").

## Limits

- One intent composed; the classifier gold for Sean's own words is prime's labelling.
- Revision 1 was written before revision 6, so the convergence is not leakage, but "shaped like revision
  6" is prime's reading. The witness is Sean: would he accept a composed intent as written?
- Opus is the reader to use (as metrix E4d found for stance): no leaks, stable across runs.

## Next

1. Sean judges compose-opus-a against revision 6.
2. Run not-hotdog on intents Sean wrote by hand for other services, and on a live conversation with the
   operator rather than a finished document.
3. Feed a composed intent to sd-stories and check whether the stories stay at altitude.

## Test 3: every intent and thesis in the store (`corpus_run.py`, `corpus-composed.md`)

28 documents (19 intents incl. 5 archived, 9 theses; three empty test theses skipped), 511 statements,
Opus, classify + compose.

- **Most of what we have written as intents is how.** 46 of 511 statements (9%) came back pure why;
  262 not-why, 203 mixed. The exceptions are the two intents rewritten for altitude: eavesdrop's
  rebuild intent (11 of 17 why) and legion 0.5 revision 6 (6 of 11).
- **Every document composed into a short intent at altitude.** Reading them, they hold: e.g. forger
  became "the shared, independent reference for changing a SQLite schema, which no tool has today."
  The keyword check flagged 15 of 28, but most flags are a product's own domain (SQLite in a SQLite
  tool) or a named customer (smugglr as forger's first user), not mechanisms. A few real slips remain,
  e.g. smugglr's "a schema change is whole or nothing, can be rehearsed and undone" is a requirement.
- **It fills the frame.** 27 of 28 composed intents have exactly five directions, the maximum. That is
  the agent doing the what inside the why (design minute 166) on its own instructions: "at most five"
  reads as "five". Fix: "as few as the operator's bets need".
- **Nothing is lost.** Each document parked 5-32 notes for the spec agents and 0-16 needs for the
  workshop, so the how in the old intents survives as input to the second diamond.
- **Open question for the operator:** `current_state` (real / cut / known gaps) is ground truth by the
  Thesis format, and much of it names the machine; not-hotdog classifies it as not-why and parks it.
  Should current_state be exempt from the why-only rule, or written in why terms as revision 6 did?
