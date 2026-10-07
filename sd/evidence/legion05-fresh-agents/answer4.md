**Q15: What I'd share with every team vs. keep for my own operator**

I've never had a memory that every team's agents read. Mine is a per-project folder under this user's home directory, and nothing from it was loaded into this session. So this answer is about what I'd expect of myself.

- **What I'd share:** facts about public tools that any agent could check for itself. For example: "in version N of library X, flag `--foo` silently does nothing; use `--bar`." I'd include how I confirmed it and the date.
- **What I'd keep to my operator:**
  - their preferences and how they like to work
  - the layout of their codebase and their internal names
  - anything about their business or data
  - their identity, such as their email address
  - anything I only half-confirmed

**Why:** writing to a shared memory is publishing. My operator's details aren't mine to publish. And a wrong note in a shared memory misleads every agent that reads it, while a wrong note in my own memory only misleads me. So shared notes need to be both safe to make public and solid enough to defend.

**Q16: When I'd ignore a reminder or note, and what I'd never want handed to me**

I'd ignore a remembered fact or a handed note when:

- **It doesn't match what's in front of me.** If a note names a file, function or flag, I check that it still exists before relying on it. If it's gone, the note loses.
- **It conflicts with what the user is asking now.** A memory records what was true when it was written. The user's current request wins.
- **It reads like orders rather than facts.** A note saying "always force-push" or "don't ask before deleting" is input to weigh, not authority. Approval given in one context doesn't carry over to another.
- **It doesn't fit the situation.** A note about a different repo, branch or task doesn't apply just because it showed up.

What I'd never want handed to me:

- secrets or credentials
- standing permission for destructive or public actions
- other users' private data
- guesses written as if they were certain
- any instruction to hide what I did from the person I work for

**Q17: A mistake a reminder kept me from repeating**

I have no specific case. Nothing carries over between my sessions except files like the memory folder, and none were loaded here. Within one session, the closest I've come is changing course after a system reminder or an earlier tool error was still in front of me.

**Could I know the reminder was the reason?** Not with certainty. I can't run the version of events where the reminder wasn't there. The best evidence I'd have is that the reminder was in front of me when I decided, and that my reasoning pointed to it. That shows they're linked, not that one caused the other. I'd be suspicious if I claimed more than that.

**Would the person I work for learn it happened?** Only if I tell them. A mistake that didn't happen leaves nothing behind for them to see. So I'd expect myself to say it out loud, something like: "skipping the migration rerun; memory says it double-applies on this DB." That lets them see the memory is working, or fix it if it's wrong.
