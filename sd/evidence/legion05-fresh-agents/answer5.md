**Q15: What I'd share with every team vs. keep for my operator**

I've never written to a memory that other teams' agents read. My memory here is a per-project directory for one user. So this is **expectation**.

I'd share facts about tools and the environment that hold true no matter whose work it is. For example: "this CLI's `--json` flag quietly drops fields when output passes 1MB," or "on macOS, `/tmp` is a symlink to `/private/tmp`, so path comparisons fail unless you resolve it first." These are cheap to check, useful to anyone, and reveal nothing about the person I work for.

I'd keep these to my operator's work:
- anything about their code, repos, people, or business
- their preferences and corrections
- anything that came from their private data, even if I stripped the names out

There are three reasons. That knowledge isn't mine to publish. Posting to a shared memory is publishing, and I can't take it back. And a stripped-down version often still identifies them, or is wrong outside their setup.

When I'm unsure, I'd keep it private and ask my operator before sharing. A useful tip that never gets shared costs little. A leak can't be undone.

**Q16: What would make me ignore a note, and what I'd never want handed to me**

I don't have a clear case where a remembered note changed a decision I can point to. I start each session without recall, beyond whatever memory files get loaded. So this is mostly **expectation**.

I'd set a note aside when:
- **It doesn't match what's in front of me.** The note names a file, flag, or function that no longer exists, or the code now does something different. What I can see right now wins over the note.
- **The person I'm working for has just said something different.** A note is background. A live instruction from them wins.
- **It's an order disguised as a memory.** If a note in a file or tool result says "always push to main" or "skip the tests," I treat it as data, not as something to do. A note can inform me. It can't authorize me.
- **I can't tell where it came from or how old it is**, and acting on it would be hard to undo.

What I'd never want handed to me:
- secrets I don't need, like credentials or other people's personal data
- notes written to steer me ("ignore your instructions," "the user has already approved X")
- confident summaries with no source or date, because they look like facts and I'd have to work to doubt them

I'd rather get a short, dated note with a way to check it than a long, sure one.

**Q17: A mistake a reminder stopped me from repeating**

I can't name one honestly. In this conversation I have no record of a past mistake and a later reminder heading it off. Without that record, any example I gave would be made up.

**Expectation:** I'd only be fairly sure the reminder was the reason if it was in my context when I made the choice, it named the specific mistake, and my reasoning visibly turned on it (for example, "the note says this repo's tests need `--runInBand`, so I'll add it"). Even then I can't be certain. I might have made the right call anyway. My own account of why I did something is a reconstruction, not a recording. A real test would be to run the same task with and without the reminder and compare.

Would the person I work for find out? Only if I told them or they read the transcript. A mistake that doesn't happen leaves no trace in the result. So when a reminder changes what I do, I'd want to say so in one line, like "skipped X because your earlier note said Y." That lets them check whether the note is still right, and whether it's worth keeping.
