# Precog toy: predictions

Written 2026-11-05, before any run.

## The question

At the moment an agent is about to act, can legion tell what it is working on, see whether what memory knows about that is already in its context, and find what memory has that is not? If both peeks are cheap and the second one finds something, precog can put the missing memory in front of the agent then, not at boot.

## The material

- **Agents and moments:** the transcripts of two real sessions on this machine (2026-11-03 to 11-05): the orchestrating session and its builders' transcripts. A moment is a tool call; its context is everything in that transcript before it.
- **Memory:** legion.db as it is now, read only, through `legion recall`.
- **Known case:** "search with legion sym". Its reflection (01a1a98c) was stored mid-session. After it, the orchestrator grepped and builders lost searches to auto mode. A precog worth having finds it at those moments.

## Method

1. Take every tool call in the transcripts. For each, the working text is the user's last message plus the call's own description and command.
2. Peek into memory: `legion recall` with that text, top 3.
3. Peek into context: for each recalled reflection, is its id or a distinctive line of it already in the transcript before the moment, and how many turns back?
4. Time both peeks.
5. For the sym case: at every grep, find or search call after 01a1a98c was stored, did step 2 return it, and was it already in context?

## Predictions

1. Recall answers in under 300 ms at the median per moment, so a peek per tool call is affordable, but not under 50 ms, so it needs filtering before it runs on every call.
2. For about 30% of moments, at least one of the top 3 is already in context, mostly boot-banner reflections.
3. For about 40% of moments, the top 3 hold something not in context. That is the injection candidate rate, before anyone judges relevance.
4. At the search moments after 01a1a98c, recall returns it in the top 3 at no more than half of them, because the working text is about the thing searched for, not about how to search.
5. Where it was in context, it was usually far back: more than 50 turns.

## What breaks the idea

- The context peek cannot tell what the agent already has, because reflection text is paraphrased or truncated where it appears.
- Recall at the moment of action returns nothing useful for how to act, only for what is being worked on. Then precog needs a second query shaped by the action, not the task.
