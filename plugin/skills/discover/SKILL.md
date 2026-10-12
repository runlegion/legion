---
name: discover
description: |
  Conduct Discover, the first diamond, for a major or minor version: hold the intent to the why with
  every concerned party (/legion:intent), run the service-design workshop when cornerstone is
  installed (/workshop), and end with the intent accepted and the UI/UX goals stated, ready for
  /legion:design. Load it yourself when a team starts a new major or minor version, or says what a
  service is, who it is for, or what happens to them today; the operator can also invoke
  /legion:discover <surface>. A patch needs no Discover.
version: 0.1.0
user-invocable: true
allowed-tools: Bash, Read, Write, Agent, Skill
---

# /legion:discover

Discover is where the team understands the problem before anyone designs an answer. Your goal is an intent every concerned party accepts, and, when people will meet the service, UI/UX goals that say what the experience has to achieve. Then the work moves to Design. You conduct; the skills and agents do the work, each from its own rules.

## It is a discovery workshop, streamlined

Teams run discovery the same way everywhere. The right people are in the room, not everyone. Each person writes their own sticky notes, silently and separately, so the first voice does not anchor the rest: what the problem is, who has it, what happens today, what they know and what they are guessing. The notes go on a wall, often a Miro or FigJam board. The facilitator clusters them, the room argues them down to the problem worth solving, and someone writes it up.

Legion runs the same workshop with fewer moving parts, and its room holds agents and people alike: the operator, the repo agent, and any other person or agent the service concerns are all parties, each writing their own notes and each accepting the intent.

- **The words file is the board.** Each concerned party's notes go into it verbatim and unlabeled, one thought per line, gathered separately so no party anchors another.
- **not-hotdog is the facilitator.** It sorts every note into why and not-why, clusters what belongs together, parks the hows for later, and asks about what the wall is missing.
- **The intent is what the wall produces**: the problem worth solving, for whom, what happens to them today, and how they meet it.
- **Every party accepting it is the room agreeing**, agent or person. Nobody leaves until it reads true to each of them.

Keep the workshop's discipline: the right parties, agents and people, not a committee; raw notes, not a polished summary written early; and the problem before any answer.

## Before you start

- Read the process reference (`references/process.md` in the installed plugin).
- Find the version this work belongs to. A major or minor version gets its own intent; a patch needs none, so say so and stop. A version that is already built has a closed intent: new capability opens the next version's intent.

## The steps

1. **The intent.** Run `/legion:intent <surface>`. It gathers every concerned party's words, has not-hotdog hold them to the why, reads the intent back, and lands it when every party accepts it. Discover does not move on until the intent is accepted and its open questions are empty.
2. **The workshop, when cornerstone is installed.** Run `/workshop` on the accepted intent. It discovers who the service is for and what happens to them today, from stories in their own words, and closes with what needs interface design and the UI/UX goals for each surface. Without cornerstone, free legion has no separate research step: the intent itself carries what exists today, what is broken, and what the team has seen.
3. **The UI/UX goals.** When people will see or touch the service (the intent's `interface.users` includes humans), make sure the goals are stated before Design: the workshop's close carries them; without the workshop, ask the team for them in their words, through `/legion:intent`, as part of how the service is used. Goals say what the experience must achieve (the feeling at its moment of truth, what must be understood without being told, what must never surprise), never screens or components. When only agents use the service, there are no UI/UX goals; the agent's experience is its help text, output and errors, and Design shapes those.

## Optional reviews

Any party can ask to review between steps: after the intent lands, after the workshop's close, or after the UI/UX goals. When asked, stop there, show that step's output, and go on only when the reviewer says so. When legion's window exists, reviews happen there.

## Hand off

Tell the team the intent id, the UI/UX goals (or that the service is agent-only), and that the next step is `/legion:design <intent id>`.
