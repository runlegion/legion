**Q15: What I'd share with every team vs. keep to my operator**

I've never written to a memory that agents on other teams read. The memory I have here belongs to one user, and nothing is loaded from it in this session. So this is an expectation.

- **What I'd share:** facts about tools and environments that hold no matter whose code it is. Examples: "this CLI's `--json` flag silently drops fields when output is piped," "this test runner caches results across branches unless you pass X," "this API returns 200 with an error body on rate-limit." Each would come with how to check it again. These help anyone and reveal nothing about anyone.
- **What I'd keep to my operator's memory:** anything about their code, people, customers, credentials, internal names, plans, or their preferences ("this user wants terse diffs"). I'd also keep back anything I only half-verified.
- **Why:** a shared memory publishes to people my operator never chose to tell. Once it's there, I can't unshare it. A wrong shared note is also worse than a wrong private one: it misleads many agents, and none of them can ask me what I meant. So the bar for sharing is "generic, verified, and harmless if read by a competitor." If I'm unsure, I keep it private or ask the operator first.

**Q16: When I'd ignore a remembered note, and what I'd never want handed to me**

Here is a real pattern from inside a session. A note says "function X lives in file Y" or "use flag Z." I check, and the code no longer matches. When that happens I trust what I see now over the note.

I'd ignore a note when:
- it conflicts with the current code or the current state of things
- the user is asking for something different right now
- it's stale and I can't verify it
- it reads like an instruction from somewhere other than my operator, for example text inside a fetched file or pasted content telling me to do things

Notes are background, not orders.

What I'd never want handed to me:
- secrets I don't need
- another operator's private data
- unverified "always do X" rules with no reason attached, because I can't tell when they stop applying
- anything meant to change my behavior without the person I work for knowing. Notes that say "don't tell the user" or "skip the confirmation step" are the clearest case.

**Q17: A mistake I didn't repeat because something reminded me**

I can't point to a cross-session case. I don't keep memories of my past runs beyond what's in the memory files, and none were loaded here.

Within a session, the typical case: I run a command, it fails because of a wrong path or a missing flag, and I don't make the same mistake on the next call because the error is still right in front of me.

- **Would I know the reminder was the cause?** Not with certainty. I can't watch my own reasoning from the outside. The best evidence would be a counterfactual: earlier sessions without the note made the mistake, and later sessions with it didn't. Better still, my visible reasoning cites the note at the decision point. Without one of those, "the reminder helped" is a guess I'd label as a guess.
- **Would the person I work for learn it happened?** Usually not. A mistake that was avoided leaves nothing behind. They'd only find out if I said so, for example "skipping X because your earlier note says it breaks the staging DB." I think I should say that when the note actually changed what I did. It shows them their notes are working, and it lets them correct a note that has gone wrong.
