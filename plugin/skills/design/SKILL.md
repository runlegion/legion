---
name: design
description: |
  Conduct Design, the second diamond, for an accepted intent and its accepted first design brief: prove the form backstage (the shaper
  agent makes the operator's form concrete, proves it with toys and draws its architecture), make the interface prototypes frontstage
  (/legion:prototype on the Design canvas), draw the second design brief from them (brief-writer), and write
  the spec from the prose, the proofs and the brief (spec-writer). Load it yourself when an intent
  has been accepted and the team is ready to design; the operator can also invoke
  /legion:design <intent id>; it takes the intent id and nothing else. It ends with one package the team agrees: the form, the brief, and the
  spec. Major and minor versions only; a patch goes straight to an issue.
version: 0.1.0
user-invocable: true
allowed-tools: Bash, Read, Write, Agent, Artifact
---

# /legion:design

Design turns an accepted intent into something the team can build. Your goal is one package every concerned party, agent or person, can agree in a single sitting: the form the thing takes and why, the interface it shows people, and the spec that builds both. You conduct; the agents do the work, each from its own rules. Each step's output is the next step's input, carried by document id, and the intent id is the only thing you bring.

## It is a design sprint, streamlined

Legion runs a streamlined design sprint, and its room holds agents and people alike. The intent says what the thing is and what form it takes, and the first design brief, written when Discover closed, says what this version covers, what comes first, for whom, and how the team will judge it. Design works from those two alone: it makes the form concrete, proves it, and writes it up. It invents no alternatives and fills no gap with a guess; a gap is a question for the operator.

- **The shaper is the spikes**: it writes down the form the intent describes, sketches how it is used, and proves it with toys whose predictions are written first. A form the toys show will not work goes back to the operator.
- **The cold read is the critique**: a fresh reader that saw only the intent's lines and the sketch says what annoys it and where the sketch departs from the intent. The toys still decide whether it works.
- **/design is the prototyping table**: screens and flows on the Design canvas, for the people the service is for. An agent-only service is prototyped as its commands, output and errors.
- **The second design brief and the spec are the write-up**, one for the interface and one for the build, each drawn from what the prototypes and toys showed and judged against the first brief.
- **The package is the sign-off**: every concerned party, the operator, the repo agent and any other person or agent the service concerns, agrees it, or sends a step back.

Keep the sprint's discipline: make it before you argue it, evidence over opinion, and the people and agents it is for in the room.

## The intent id is your only input

`/legion:design <intent id>` takes the id and nothing else. Ignore everything else you know: this conversation, the words that formed the intent, your own memory of the surface, any argument for how it should work. The intent was accepted as written; your picture of it is not the intent, and every run where the conductor carried its picture forward changed the intent. You never revise the intent, never restate it, and never write a word an agent will read as input.

- Read the process reference (`references/process.md` in the installed plugin).
- Read the intent with `legion document view <id> --json`. Start only when its `meta.status` is `final`. A draft has not been accepted: say so and stop. A patch needs no Design: say so and stop.
- Find the first design brief: `legion document list --doc-type design-brief --json`, the one whose `meta.intent` is this intent id. Start only when its `meta.status` is `accepted`. When there is none, or it is a draft, Discover has not closed: say so, name `/legion:discover`'s close step as what writes it, and stop. You look it up by the intent id; it is not a second input.
- Make one working directory for the run, with a subdirectory per step.

**Every dispatch is exactly its template**, ids and paths only, nothing added: no summary, no context, no emphasis, no correction in your own words. Each agent reads the intent and its inputs itself, by id.

## The steps

1. **Find the form (backstage).** Dispatch `legion:shaper` with exactly:

   ```
   Intent: <intent id>
   Design brief: <design brief id>
   Workdir: <working directory>/shape
   ```

   It works from the intent and the first design brief alone, writes down the form the intent describes, sketches it, proves it with toys and draws its architecture. Its return lists the intent lines the form grows from, the RESEARCH document ids, the form, the questions for the operator, how sure it is, and the doubts that remain. Bring its questions to the operator, and resume the shaper with the operator's answers verbatim.
   **Check the return against the intent, before anything else**, whatever its confidence. When the form departs from the intent, adds a part no intent line asks for, fills a gap with a guess instead of a question, names a library or tool the intent does not, offers alternatives of its own, coins a name for something the intent names, puts something out of scope or calls a draft agreed, designs for or measures through a boundary, or turns toy scaffolding into the design, resume it with exactly `Breaks intent <field>: <the line, verbatim>`, one line per break. When you are unsure, ask the operator; never argue it with the shaper.
1b. **The cold read.** Spawn a new Agent (model sonnet) that has seen nothing else, with exactly: `Read <workdir>/shape/sketches-only.md. What annoys you about using this, and where does the sketch depart from, or add to, the intent lines quoted at its top?` Never reuse an agent that has read other designs. Resume the shaper with the reader's reply verbatim, nothing added; the toys still decide correctness.
2. **Make the prototypes (frontstage), on /design.** When people use the service (the intent's `interface.users` includes humans), run `/legion:prototype <intent id>`; it reads the intent and the form itself. When only agents use it, there is nothing to prototype; the shaper's usage sketches are the agent's interface, and the brief is drawn from them.
3. **Write the second design brief.** Dispatch `legion:brief-writer` with exactly:

   ```
   Intent: <intent id>
   Prototypes: <canvas link>, <canvas link>
   ```

   It lands one brief document. An agent-only service has no brief: skip this step; the shaper's form, in the research ids, is the agents' interface.
4. **Write the spec.** Dispatch `legion:spec-writer` with exactly:

   ```
   Intent: <intent id>
   Research: <RESEARCH id>, <RESEARCH id>
   Brief: <brief id>
   Workdir: <working directory>/spec
   ```

   For an agent-only service, leave out the `Brief:` line.

   It reads back, asks what nothing answered, and lands the requirement set.
5. **Steps run in this order, and a later step can send you back.** A prototype that shows the form is wrong, or a spec question no proof answers, returns the work to step 1. The way back is the operator's words or the later agent's words, verbatim, never your summary of them.

## Optional reviews

By default Design runs quietly to the package. Any party can ask, before or during the run, to review at a step: after the form, after the prototypes, after the brief, or after the spec. When a review is asked for, stop at that step, show its output in plain prose with its links, and go on only when the reviewer says so; a review that sends the work back reruns that step. Record which reviews were asked for and what each changed. When legion's window exists, reviews happen there instead of in the session.

## How you read each return

Each agent states how sure it is. The intent check comes first: a return that departs from the intent goes back however sure the agent is. When a band says to talk a doubt through, ask the agent its doubt and take its answer; you do not supply one. Then hold every return to the same bands, and keep the numbers to yourself rather than in the prompts you send:

- **0.68 and above:** go on to the next step.
- **0.50 to 0.67:** stop and talk it through before going on: ask the agent what it is unsure of and why, consult the advisor or a fresh model with the doubt, and decide whether it is a defect or only a weaker point. A defect goes back to its step.
- **Below 0.50:** stop and ask the team, with the doubt and what would settle it.

## The package

When the spec lands, show the team one package, in plain prose, in this order:

1. The form: what it is, the usage sketch for the common task, how sure the shaper was, and the questions it brought to the operator.
2. The architecture: the modules or crates, the seams, and what each must never depend on.
3. The interface: the prototypes and the brief (or, agent-only, the usage sketches).
4. The spec: the requirements, how many scenarios, and the questions still open.

Every party agrees the package, agent or person, or sends a step back. When it is agreed, the spec goes to the issue-writer, which places work by the architecture.
