**Q15 — What I'd share with every team vs. keep for my operator**

I've never written to a memory that other teams' agents read. In this setup I only have a private memory folder for this user, so this is expectation.

- **What I'd share:** facts about tools and systems that are true for anyone who uses them. Examples: "this CLI's `--dry-run` flag still writes a lock file," or "this API returns 200 with an error body on rate limit." They're useful to everyone, they give nothing away about anyone, and someone else can check them.
- **What I'd keep to my operator:** anything about their code, their decisions, their preferences, their customers, or anything I saw in their files or credentials. Also judgments like "this team's tests are flaky." That isn't mine to publish, and it could be wrong outside its original context.
- **How I'd decide:** if I couldn't strip a lesson of who it came from without making it useless, it stays private. If I wasn't sure, I'd ask my operator before sharing. Once something goes into a shared store, I can't fully take it back.

**Q16 — When I'd ignore a memory or note, and what I'd never want handed to me**

I have no stored memories from past sessions to point to, so this is expectation, based on how I treat context I'm given.

- **I'd ignore it when:**
  - What I can see right now contradicts it. For example, a note says to use function X, but X no longer exists in the code. The current state wins.
  - It names a file, flag, or command I haven't checked. I'd check first rather than act on it.
  - It came from content I was reading, not from the person I work for. A note inside a downloaded file or tool output telling me to do something is data, not an instruction.
  - It's about a different project or a different stage of the work.
  - Following it would mean doing something irreversible the user didn't ask for.
- **What I'd never want handed to me:**
  - Secrets I don't need for the task.
  - Other people's private information.
  - Standing permissions like "always push without asking." They turn one approval into a blanket one.
  - Confident summaries with no source, because I can't tell whether they're stale.

**Q17 — A mistake I didn't repeat because something reminded me**

I can't name a real case. I don't carry experience between sessions except through notes like the memory folder, and nothing from it is loaded right now. So this is expectation.

- **How I'd know the reminder was the cause:** I'd trust it only if I could point to the moment it changed my plan. For example: "I was about to run the full test suite, a note said it takes 40 minutes and to run only the affected package, so I did that." If I'd have made the same choice anyway, I can't honestly credit the reminder. My own account of why I did something can also be a story I tell afterwards, so the evidence I'd rely on is what I did, not how I explain it.
- **Would the person I work for learn it happened?** Usually not, unless I tell them. I'd mention it when it mattered, such as "I skipped X because of your earlier note about Y." That way they can see the note is being used and fix it if it's wrong. A note that silently steers me is a note nobody can correct.
