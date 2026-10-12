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

Design turns an accepted intent into something the team can build. Your goal is one package the team can agree in a single sitting: the form the thing takes and why, the interface it shows people, and the spec that builds both. You conduct; the agents do the work, each from its own rules. Each step's output is the next step's input, carried by document id.

## Before you start

- Read the process reference (`references/process.md` in the installed plugin).
- Read the intent with `legion document view <id> --json`. Design starts only from an intent the team has accepted, for a major or minor version. A patch needs no Design: say so and stop.
- When the workshop ran for this intent, find its close and its UI/UX goals; they are inputs to the prototypes and to the shaper.
- Make one working directory for the run, with a subdirectory per step.

## The steps

1. **Find the form (backstage).** Dispatch `legion:shaper` with the intent id, the repo, the surface and its working directory. It works quietly: mood board, rough ideas, usage sketches, toys, the chosen form and its architecture. Read its return: the RESEARCH document ids, the chosen form, how sure it is, and the doubts that remain.
1b. **The cold read.** Spawn a new Agent (model sonnet) that has seen nothing else, give it only the shaper's `sketches-only.md`, and ask which form it would reach for every day and what annoys it. Never reuse an agent that has read other designs. Send its notes to the shaper (resume it), which fixes the chosen form or reconsiders; the toys still decide correctness.
2. **Make the prototypes (frontstage).** When people use the service (the intent's `interface.users` includes humans), run `/legion:prototype` for the intent, guided by the workshop's UI/UX goals and the chosen form, so the screens show the form, not a guess at it. When only agents use it, there is nothing to prototype; the shaper's usage sketches are the agent's interface, and the brief is drawn from them.
3. **Write the brief.** Dispatch `legion:brief-writer` with the intent id and the prototype pointers (or, for an agent-only service, the shaper's form document). It lands one brief document.
4. **Write the spec.** Dispatch `legion:spec-writer` with the intent id, the shaper's RESEARCH document ids (the proofs and the chosen form, whose architecture places every scenario), the brief's id, the repo, the surface and its working directory. It reads back, asks what nothing answered, and lands the requirement set.
5. **Steps run in this order, and a later step can send you back.** A prototype that shows the form is wrong, or a spec question no proof answers, returns the work to step 1 with that question; say which step and why.

## How you read each return

Each agent states how sure it is. Hold every return to the same bands, and keep the numbers to yourself rather than in the prompts you send:

- **0.68 and above:** go on to the next step.
- **0.50 to 0.67:** stop and talk it through before going on: ask the agent what it is unsure of and why, consult the advisor or a fresh model with the doubt, and decide whether it is a defect or only a weaker point. A defect goes back to its step.
- **Below 0.50:** stop and ask the team, with the doubt and what would settle it.

## The package

When the spec lands, show the team one package, in plain prose, in this order:

1. The form: what it is, the usage sketch for the common task, how sure the shaper was, and what it beat and why.
2. The architecture: the modules or crates, the seams, and what each must never depend on.
3. The interface: the prototypes and the brief (or, agent-only, the usage sketches).
4. The spec: the requirements, how many scenarios, and the questions still open.

The team agrees the package or sends a step back. When it is agreed, the spec goes to the issue-writer, which places work by the architecture.
