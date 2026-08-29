/**
 * usePresence -- the presence mechanism (#1996, rewritten #2157).
 *
 * THE CONTRACT, ratified 2026-08-02:
 *   enter = the node arrives with a keyframe animation already attached, and
 *           runs it. An element that MOUNTS (or leaves `display: none`) with an
 *           animation needs no `@starting-style` -- that hack exists for
 *           TRANSITIONS on mount, and this system does not use it anywhere.
 *   exit  = the state flips to 'closed', the exit keyframe runs, and REMOVAL IS
 *           HELD by the behavior layer until the animation ends. Presence owns
 *           the unmount; CSS owns the movement.
 *
 * Callers render while `present` is true and attach `ref` to the animated node.
 * That node's classes key the enter/exit keyframes off `data-[state=open]` /
 * `data-[state=closed]`.
 *
 * JS OBSERVES, IT DOES NOT TIME. This hook asks the node what is running --
 * `getAnimations()` -- and waits on the animations' own `finished` promises. It
 * does not read a duration out of `getComputedStyle`, does not add a margin to
 * it, and does not run a `setTimeout` against the result. The browser already
 * knows exactly when the exit is over; parsing CSS time strings in TypeScript to
 * guess at the same number was a second, worse clock running alongside the real
 * one.
 *
 * WHO WRITES `data-state`. Not this hook, for any caller that composes the
 * `disclosable` slice -- dialog, popover, dropdown-menu all do. `disclosable`
 * already contributes `data-state` to the content part from
 * `isOpen(state, config)`, and that is the SAME value the caller passes here as
 * `open`, so the two are equal on every render, exit window included: while the
 * node is closing, `open` is false and `present` is true, and both writers say
 * `closed`. There is nothing to reconcile, so there is no reason to have two.
 * The attribute has one writer, and it is disclosable.
 *
 * `state` is returned for callers OUTSIDE that composition (a bare
 * `usePresence` on a node with no behavior contract behind it), which is why it
 * remains part of the interface rather than being deleted.
 *
 * THE THREE WAYS THIS COULD WEDGE, and what stops each:
 *   1. Nothing is attached -- a transition declared on a property that never
 *      changes creates no `CSSTransition` at all, so nothing will ever finish.
 *      -> `getAnimations()` comes back EMPTY, and presence releases in the same
 *      effect, synchronously. There is no event to miss and no timer to run.
 *   2. Reduced motion. The generated cell utilities zero `animation-duration`
 *      under `prefers-reduced-motion` (mechanism B, #2017), and a ZERO-DURATION
 *      animation is never handed back by `getAnimations()` -- it finishes inside
 *      the same style flush that creates it, which makes it no longer relevant,
 *      so the list comes back empty and the exit releases in the same tick as
 *      case 1. Measured, not assumed: `test/presence/presence-race.e2e.ts` pins
 *      it against the real engines, and the same is true of a bare
 *      `data-state` flip with no React in the picture at all.
 *
 *      That is the correct outcome, and it is why the old event-based code
 *      needed a special case here and this does not. Under reduced motion the
 *      exit is over before a frame could paint, so there is nothing to hold the
 *      node for; the old implementation had to enter its wait anyway because it
 *      released on `animationend`, and an `animationend` that had already fired
 *      would have wedged the node until the backstop timer. There is no event to
 *      miss here, so there is no reason to wait for one.
 *   3. Rapid open -> close -> open. A pending release from the previous close
 *      would land on the node that is now legitimately open. -> the effect's
 *      cleanup sets `cancelled`, and a late-settling wait finds it set and does
 *      nothing.
 *
 * The interrupted enter needs no special handling any more. Closing mid-enter
 * CANCELS the enter animation, and a cancelled animation is not a running one:
 * `getAnimations()` does not return it. Even if an engine did hand it back, its
 * `finished` REJECTS on cancellation and `Promise.allSettled` absorbs that
 * without collapsing the wait -- which is the whole reason `allSettled` is used
 * here instead of `Promise.all`. The old code needed an animation-name filter to
 * tell the dying enter's `animationcancel` apart from the exit's `animationend`;
 * with promises there is no shared event channel to disambiguate.
 *
 * NO BACKSTOP TIMER, and none is needed. The one shape that would never settle
 * is an infinite loop animation on the presence node, and no such cell exists:
 * every `period-*` row in `docs/spec/matrix/motion.jsonl` belongs to input-otp's
 * caret, progress, skeleton, or spinner, and not one of those is presence-gated.
 * If a loop ever lands on an overlay content part, THAT is when a backstop is
 * warranted -- and it would be an engineering failsafe outside the value system,
 * never derived from a duration tier or any other token.
 *
 * The score is untouched: presence is a pure DOM-lifecycle concern (when to
 * keep a node), not behavior. The WC and Astro performances share the mechanism
 * through the same `hidden`-attribute shape the React dropdown-menu uses.
 *
 * @example
 * const { present, ref, state } = usePresence(open);
 * return present ? <div ref={ref} data-state={state} /> : null;
 */
import * as React from "react";

export type PresenceState = "open" | "closed";

export interface Presence {
  /** Render the node while true; it stays true through the exit animation. */
  present: boolean;
  /** Attach to the animated node so presence can watch its animation end. */
  ref: (element: HTMLElement | null) => void;
  /**
   * 'open' | 'closed' -- the data-state the exit CSS keys off. Callers composing
   * `disclosable` must NOT put this on the node: that slice already contributes
   * the identical value. For callers with no behavior contract behind them, this
   * is the attribute's source. See the ownership note above.
   */
  state: PresenceState;
}

export function usePresence(open: boolean): Presence {
  const [present, setPresent] = React.useState(open);
  const nodeRef = React.useRef<HTMLElement | null>(null);

  // Open SYNCHRONOUSLY (React's adjust-state-during-render pattern): the node
  // must mount in the same commit as the open dispatch, so effects that
  // resolve it by id (focus-trap) find it, and so it mounts WITH its enter
  // animation attached rather than a frame later. Only the exit is deferred.
  if (open && !present) setPresent(true);

  React.useEffect(() => {
    if (open) return;
    const node = nodeRef.current;
    if (!node) {
      setPresent(false);
      return;
    }
    // An environment guard, not a policy one. Test DOMs (happy-dom, jsdom) ship
    // no Web Animations API at all, and a hook that threw there would take every
    // consumer's suite down with it. Where there is no animation timeline there
    // is also nothing to observe, so the honest answer is the same as case 1:
    // release now. This is a DOM-shim check -- it is not a tunable and it is not
    // a timing failsafe.
    if (typeof node.getAnimations !== "function") {
      setPresent(false);
      return;
    }
    // Read AFTER the data-state=closed commit that already happened this render,
    // so these are the EXIT animations, not a stale reference to a still-running
    // enter. `getAnimations()` flushes pending style before it answers, which is
    // what makes the freshly-applied exit rule visible here.
    const animations = node.getAnimations();
    if (animations.length === 0) {
      // Nothing attached and nothing coming -- case 1 above. Release now rather
      // than await a promise that will never be created.
      setPresent(false);
      return;
    }

    let cancelled = false;
    const releaseWhenSettled = async () => {
      // allSettled, not all: a cancelled or replaced animation REJECTS its
      // `finished`, and that rejection is an outcome, not a failure. Absorbing
      // it is the point.
      await Promise.allSettled(animations.map((animation) => animation.finished));
      if (!cancelled) setPresent(false);
    };
    void releaseWhenSettled();

    return () => {
      // Reopening mid-exit lands here: the pending wait still settles, finds
      // this flag, and leaves the reopened node alone.
      cancelled = true;
    };
  }, [open]);

  const ref = React.useCallback((element: HTMLElement | null) => {
    nodeRef.current = element;
  }, []);

  return { present, ref, state: open ? "open" : "closed" };
}
