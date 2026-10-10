# The legion process

Legion's process exists so that a team gets from a problem it has seen to working
software it can trust, without losing the thread between the two. It runs in four
stages, in this order: Discover, then Design, then Work, then Archive.

## Discover

Discover produces the intent: why this exists, for whom, and what happens to them
today. Free legion has no separate research step, so the intent itself carries what
exists today, what is broken, and what the team has seen. Nothing else is gathered
before the intent; if the intent cannot say it, the team has not yet seen it.

## Design

Design turns the intent into something the team can build. Three kinds of work run
together, each informing the others:

- **Prototypes.** Interface prototypes, made with the Design canvas (Claude's
  /design, always available to legion). They show the interface before anyone
  builds it.
- **Proofs.** Backstage toys that test whether the machinery works. A toy lives in
  a scratchpad and never reaches main; what it leaves behind is the answer, not the
  code.
- **The documents.** The prose that says what the service is and must do.

Design ends with two outputs: a design brief for the interfaces, drawn from the
prototypes, and the spec, drawn from the prose and the proofs.

## Work

Work takes the spec through issues to code complete.

## Archive

Archive follows Work.

## The team

The team is the operator and the repo agent together. Both accept the intent, and
both agree the spec.
