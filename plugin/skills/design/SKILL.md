---
name: design
description: |
  Conduct Design, the second diamond, for an accepted intent: find the form backstage (the shaper
  agent's mood board, rough ideas, toys and architecture), make the interface prototypes frontstage
  (/legion:prototype on the Design canvas), draw the design brief from them (brief-writer), and write
  the spec from the prose, the proofs and the brief (spec-writer). Load it yourself when an intent
  has been accepted and the team is ready to design; the operator can also invoke
  /legion:design <intent id>. It ends with one package the team agrees: the form, the brief, and the
  spec. Major and minor versions only; a patch goes straight to an issue.
version: 0.1.0
user-invocable: true
allowed-tools: Bash, Read, Write, Agent, Artifact
---

# /legion:design

Design turns an accepted intent into something the team can build. Your goal is one package every concerned party, agent or person, can agree in a single sitting: the form the thing takes and why, the interface it shows people, and the spec that builds both. You conduct; the agents do the work, each from its own rules. Each step's output is the next step's input, carried by document id.

## It is a design sprint, streamlined

Teams run design the same way everywhere. They open out first: a mood board of what others have done, several rough concepts sketched side by side, and the one that departs from all of them on purpose. Then they make things small and test them: paper prototypes and clickable mockups in front of the people they are for, spikes and proofs of concept for the machinery. A critique tears the concepts apart, the strongest survives, and the team converges: the design is written up as a brief for whoever builds the interface and a spec for whoever builds the rest, and the room signs off.

Legion runs the same sprint with fewer moving parts, and its room holds agents and people alike:

- **The shaper is the open-out and the spikes**: the mood board, the rough concepts (one is always "adopt what exists", one departs from the references), the usage sketches, and toys with predictions written first. It opens out inside the operator's choices, the way a sprint team explores within the brief it was given, never around it.
- **The cold read is the critique**: a fresh reader that saw only the sketches says what it would use every day and what annoys it. The toys still decide whether a concept works.
- **/design is the prototyping table**: screens and flows on the Design canvas, for the people the service is for. An agent-only service is prototyped as its commands, output and errors.
- **The brief and the spec are the write-up**, one for the interface and one for the build, each drawn from what the prototypes and toys showed.
- **The package is the sign-off**: every concerned party, the operator, the repo agent and any other person or agent the service concerns, agrees it, or sends a step back.

Keep the sprint's discipline: several real concepts before one, make it before you argue it, evidence over opinion, and the people and agents it is for in the room.

## Before you start

- Read the process reference (`references/process.md` in the installed plugin).
- Read the intent with `legion document view <id> --json`. Design starts only from an intent the team has accepted, for a major or minor version. A patch needs no Design: say so and stop.
- When the workshop ran for this intent, find its close and its UI/UX goals; they are inputs to the prototypes and to the shaper.
- Make one working directory for the run, with a subdirectory per step.
- **Write the givens.** The shaper does what it is not told not to do, so tell it. Recall the operator's recorded choices for the surface. Recall ranks and has no tag filter, and the default five results miss them, so cast wide and read every card:

  ```bash
  legion recall --repo <repo> --context "<surface> current choice" --limit 25 --tier card
  legion recall --repo <repo> --context "<surface> operator chose form" --limit 25 --tier card
  legion recall --repo <repo> --context "Givens for Design from intent <intent id>" --limit 10 --tier card
  legion recall --repo <repo> --context "Research for intent <intent id>" --limit 10 --tier card
  ```

  Pull the full text of each card that records a choice with `legion recall --repo <repo> --id <id>`. Then write `givens.md` in the working directory:
  - **The operator's choices**, verbatim with their reflection ids: every form, tool or mechanism the operator chose. These are the starting point the shaper explores within, never options it competes against.
  - **What is fixed**: what is already built and agreed, with its evidence (a merged PR, an agreed spec). A draft nobody agreed is not fixed. Nothing is out of scope unless the operator said so.
  - **The boundaries**: every consumer and neighbour the intent names, each owned by someone else. The shaper never designs for another repo's code or measures success through it.
  - **The research**: each "I don't know" from the intent, marked code (the shaper's toys) or design (/legion:prototype or design research).
  Show the givens to the operator in plain prose, briefly, and go on when they say so or correct it. A shaper sent without givens invents them.

## The steps

1. **Find the form (backstage).** Dispatch `legion:shaper` with the intent id, the repo, the surface, its working directory and the path to `givens.md`. It works quietly: mood board, rough ideas, usage sketches, toys, the chosen form and its architecture. Read its return: the RESEARCH document ids, the chosen form, how sure it is, and the doubts that remain.
   **Check it against the givens before anything else**, whatever its confidence. Send it back, naming the given, when the chosen form replaces or demotes an operator's choice, when it coins a name for something the operator already named, when it put something out of scope or called a draft agreed, when it designs for or measures through a boundary, or when a structure a toy needed (a dialect, a workflow, a store) became the design without the operator's words calling for it.
1b. **The cold read.** Spawn a new Agent (model sonnet) that has seen nothing else, give it the shaper's `sketches-only.md` and the operator's choices from `givens.md`, and ask which form it would reach for every day, what annoys it, and where a sketch departs from what the operator chose. A reader shown only the shaper's sketches ratifies whatever it is shown. Never reuse an agent that has read other designs. Send its notes to the shaper (resume it), which fixes the chosen form or reconsiders; the toys still decide correctness.
2. **Make the prototypes (frontstage), on /design.** When people use the service (the intent's `interface.users` includes humans), run `/legion:prototype` for the intent, guided by the workshop's UI/UX goals and the chosen form, so the screens show the form, not a guess at it. When only agents use it, there is nothing to prototype; the shaper's usage sketches are the agent's interface, and the brief is drawn from them.
3. **Write the brief.** Dispatch `legion:brief-writer` with the intent id and the prototype pointers (or, for an agent-only service, the shaper's form document). It lands one brief document.
4. **Write the spec.** Dispatch `legion:spec-writer` with the intent id, the shaper's RESEARCH document ids (the proofs and the chosen form, whose architecture places every scenario), the brief's id, the repo, the surface and its working directory. It reads back, asks what nothing answered, and lands the requirement set.
5. **Steps run in this order, and a later step can send you back.** A prototype that shows the form is wrong, or a spec question no proof answers, returns the work to step 1 with that question; say which step and why.

## Optional reviews

By default Design runs quietly to the package, with one fixed stop: the givens read-back before the shaper is dispatched. Any party can ask, before or during the run, to review at a step: after the form, after the prototypes, after the brief, or after the spec. When a review is asked for, stop at that step, show its output in plain prose with its links, and go on only when the reviewer says so; a review that sends the work back reruns that step. Record which reviews were asked for and what each changed. When legion's window exists, reviews happen there instead of in the session.

## How you read each return

Each agent states how sure it is. The givens check comes first: a return that breaks a given goes back however sure the agent is. Then hold every return to the same bands, and keep the numbers to yourself rather than in the prompts you send:

- **0.68 and above:** go on to the next step.
- **0.50 to 0.67:** stop and talk it through before going on: ask the agent what it is unsure of and why, consult the advisor or a fresh model with the doubt, and decide whether it is a defect or only a weaker point. A defect goes back to its step.
- **Below 0.50:** stop and ask the team, with the doubt and what would settle it.

## The package

When the spec lands, show the team one package, in plain prose, in this order:

1. The form: what it is, the usage sketch for the common task, how sure the shaper was, and what it beat and why.
2. The architecture: the modules or crates, the seams, and what each must never depend on.
3. The interface: the prototypes and the brief (or, agent-only, the usage sketches).
4. The spec: the requirements, how many scenarios, and the questions still open.

Every party agrees the package, agent or person, or sends a step back. When it is agreed, the spec goes to the issue-writer, which places work by the architecture.
