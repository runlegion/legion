# Board-derived goal: native `/goal` is not programmatically settable

Date: 2026-05-30
Issue: #525 (board-derived /goal -- set native completion-condition from the active card)
Parent epic: #511 (autonomy loop)

## Question

#525 asked legion to set Claude Code's native `/goal` automatically from the active kanban
card, so continuation is board-driven instead of the operator typing `/goal`.

## Finding

**There is no programmatic way to set the native `/goal`.** Verified (claude-code-guide,
against CC v2.1.154 docs, `claude --help`, the hook-output schema, and settings schema):

- No hook event output sets it -- `hookSpecificOutput` exposes `additionalContext`,
  `systemMessage`, `permissionDecision`, `sessionTitle`, but no goal field.
- No `--goal` CLI flag, no settings.json key, no env var.
- `/goal` is user-typed only.

Mechanism (useful to know): **`/goal` is itself a session-scoped, prompt-type Stop hook.**
After each turn a fast model evaluates "is the condition met?" and auto-continues until yes.
It shipped in v2.1.139; surface unchanged through v2.1.154.

## Decision

#525's own AC anticipated this ("degrades silently when /goal is unavailable"). Since native
`/goal` is *always* unavailable to a hook, the degrade path is the only path, and legion ships
the supported **equivalent** rather than the native set:

- **The board is the goal state.** `legion goal --repo <r>` derives the completion condition
  from the active *Accepted* card's acceptance criteria (`kanban::format_active_goal`). There is
  no separate goal to set or clear -- it is re-derived from board state every time.
- **Set on accept:** `legion kanban accept` echoes the accepted card's AC as the goal.
- **Carried across turns/sessions:** SessionStart emits `legion goal` so a fresh session opens
  with the active card's AC as its stated completion condition.
- **Clears on terminal/blocked:** once the card is no longer Accepted, `legion goal` returns
  empty. No state to reset.
- **Enforcement** (the auto-continue half that native `/goal` does via its prompt Stop hook) is
  provided by legion's existing kanban Stop gate (#523): it blocks stopping while a card is
  Accepted. So the board-derived goal states *what* "done" means; the Stop gate keeps the agent
  going until the card leaves Accepted. The two together are legion's `/goal`.

## Not done

A prompt-type Stop hook that evaluates the card's AC per turn (the closest literal clone of
`/goal`'s auto-continue) is not added: hooks.json prompts are static and cannot inject the
current card's AC at runtime, and the command-type Stop gate (#523) already provides
board-derived continuation. If CC later exposes a goal-set API, `legion goal` is the natural
place to call it.
