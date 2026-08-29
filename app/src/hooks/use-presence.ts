/**
 * usePresence -- the presence mechanism (#1996).
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
 * THE THREE WAYS THIS WEDGES, and what stops each:
 *   1. The animation is cancelled, or `animationend` never arrives at all (a
 *      transition declared on a property that never changes fires nothing).
 *      -> a timeout fallback, DERIVED from the node's own computed duration and
 *      delay, releases the unmount. Derived, not a constant: a magic 500ms
 *      would pin a closed dialog on screen under a slow intent and truncate a
 *      slower one.
 *   2. Reduced motion. The generated cell utilities zero `animation-duration`
 *      under `prefers-reduced-motion` (mechanism B, #2017) -- the animation is
 *      still ATTACHED and still completes, instantly, and still fires
 *      `animationend`. So the wait IS entered and the normal event path
 *      releases; the exit is simply over in the same frame. An element with no
 *      animation and no transition at all is the separate case, and that one
 *      releases synchronously because nothing will ever fire.
 *   3. Rapid open -> close -> open. A pending release from the previous close
 *      would land on the node that is now legitimately open. -> the listeners
 *      AND the timer are torn down by the same cleanup, which runs on every
 *      change of `open`.
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

/**
 * Milliseconds in a CSS time string, or 0 for anything that is not one.
 * '0.2s' -> 200, '200ms' -> 200, '0s' / '' / undefined -> 0.
 */
function timeMs(value: string): number {
  const parsed = Number.parseFloat(value);
  if (!Number.isFinite(parsed) || parsed <= 0) return 0;
  return value.trim().endsWith("ms") ? parsed : parsed * 1000;
}

/** The longest duration+delay across a comma-separated CSS time list. */
function longestRun(durations: string, delays: string): number {
  const durationParts = durations.split(",");
  const delayParts = delays.split(",");
  let longest = 0;
  for (const [index, duration] of durationParts.entries()) {
    // CSS repeats the shorter list; an absent delay is 0 either way.
    const delay = delayParts[index % (delayParts.length || 1)] ?? "";
    longest = Math.max(longest, timeMs(duration) + timeMs(delay));
  }
  return longest;
}

interface ExitMeasurement {
  /** How long the exit will run, in ms; 0 means nothing is running. */
  runMs: number;
  /**
   * The exit animation's names -- EVERY name in the computed `animation-name`
   * list -- or empty when the exit is a transition (or nothing).
   *
   * A list, not a single string, because `animation-name` is a comma-separated
   * CSS list and a node may legitimately run several exit keyframes at once
   * (`animation-name: scale-out, fade-out`). Each fires its own `animationend`
   * carrying its own single name, so comparing the event against the raw
   * computed string would match NONE of them and the exit would only ever be
   * released by the backstop timer -- a visible extra beat on every close.
   *
   * Presence must know these, not just the duration. When a close interrupts a
   * RUNNING ENTER, the browser cancels the enter animation and fires
   * `animationcancel` on the same node -- and a handler that releases on any
   * animation event unmounts the overlay instantly, on the enter's death rather
   * than the exit's completion. That is a race (it only fires if the enter was
   * still running), it truncates the exit to nothing, and it was invisible in
   * jsdom: it took watching a real browser to see it. The names are the filter.
   */
  names: string[];
}

/** The trimmed, non-empty members of a comma-separated CSS list. */
function cssList(value: string): string[] {
  return value
    .split(",")
    .map((part) => part.trim())
    .filter((part) => part.length > 0);
}

/**
 * The animation name an event carries, or null when it does not carry one --
 * a transitionend, or a bare Event. Null means "unidentified", and an
 * unidentified end is honoured rather than filtered out: the filter exists to
 * reject a DIFFERENT, named animation, not to demand identification.
 */
function animationNameOf(event: Event): string | null {
  if (typeof AnimationEvent === "undefined" || !(event instanceof AnimationEvent)) return null;
  return event.animationName;
}

/**
 * Measure the exit the node is about to run.
 *
 * A zero `runMs` does NOT mean "nothing will fire". Under reduced motion the
 * cell utilities zero `animation-duration` while leaving the animation attached
 * (mechanism B, #2017), so the exit runs for 0ms and `animationend` arrives
 * anyway. `names` is therefore claimed whenever an animation is ATTACHED, not
 * only when it has a duration -- the pair (runMs 0, names non-empty) is exactly
 * the reduced-motion exit, and it must be waited on rather than released blind.
 * Only (runMs 0, names empty) means nothing is coming.
 */
function measureExit(element: HTMLElement): ExitMeasurement {
  if (typeof getComputedStyle !== "function") return { runMs: 0, names: [] };
  const style = getComputedStyle(element);
  const animationName = style.animationName || "none";
  // `none` is also a legal MEMBER of the list ('none, scale-out'), so filter it
  // out per-member rather than only rejecting the whole string.
  const names = cssList(animationName).filter((name) => name !== "none");
  const animated =
    names.length > 0 ? longestRun(style.animationDuration || "", style.animationDelay || "") : 0;
  const transitioned =
    (style.transitionProperty || "none") !== "none"
      ? longestRun(style.transitionDuration || "", style.transitionDelay || "")
      : 0;
  return {
    runMs: Math.max(animated, transitioned),
    // Only claim names when the animation is what we are actually waiting on.
    // `animated > 0` was the old guard and it dropped the reduced-motion exit on
    // the floor: a zeroed animation-duration made `animated` 0, the names went
    // empty, and presence released synchronously without ever listening.
    names: animated >= transitioned && names.length > 0 ? names : [],
  };
}

/**
 * Margin on the derived fallback. The timer is a BACKSTOP, not the schedule --
 * it must never beat a healthy animation to the punch, or every exit truncates.
 * One frame of slack plus a small proportional allowance covers compositor
 * jitter without holding a wedged node around for a perceptible extra beat.
 *
 * THESE TWO CONSTANTS ARE AN ENGINEERING FAILSAFE AND SIT OUTSIDE THE VALUE
 * SYSTEM. State it plainly, because #2012 shipped them unclassified and a
 * reviewer was right to call that a defect: every OTHER number in motion is
 * either a perceptual fact or a designer's personality, and those belong in the
 * five namespaces where Studio can retune them. The x1.5 and the +50 are
 * neither. They exist only for the case where `animationend` never arrives -- an
 * animation that is replaced rather than cancelled, or a transition declared on
 * a property that never changes -- and in EVERY healthy exit, including the
 * zero-duration reduced-motion one, the event wins and this timer is cleared
 * before it fires. Nothing here is ever perceived as motion, so there is nothing
 * here to tune. If a user can feel this number, the animation was already broken
 * and the number is what kept the app usable.
 *
 * The consequence of that classification: do not promote these to tokens, do not
 * derive them from a duration tier, and do not let an intent change reach them.
 */
function fallbackMs(runMs: number): number {
  return Math.ceil(runMs * 1.5) + 50;
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
    // Closing: the re-render already applied data-state=closed, so the exit CSS
    // is running -- hold present until it ends, then release to unmount.
    const node = nodeRef.current;
    if (!node) {
      setPresent(false);
      return;
    }
    const { runMs, names: exitNames } = measureExit(node);
    if (runMs === 0 && exitNames.length === 0) {
      // Nothing is attached, so nothing will ever fire -- release now rather
      // than wait on an event that is not coming. NOT the reduced-motion case:
      // there an animation IS attached at zero duration, it completes in the
      // same frame, and it fires `animationend` like any other. That path falls
      // through to the listener below on purpose (#2017) -- releasing it here
      // would silently take the presence contract off the event and onto
      // whatever happened to run first.
      setPresent(false);
      return;
    }

    let timer: ReturnType<typeof setTimeout> | undefined;
    const release = () => {
      if (timer !== undefined) clearTimeout(timer);
      timer = undefined;
      setPresent(false);
    };
    const done = (event: Event) => {
      if (event.target !== node) return; // ignore descendant animations
      // ...and ignore the DYING ENTER. Interrupting a running enter cancels it,
      // which fires animationcancel on this very node; releasing on that ends
      // the exit before its first frame paints.
      const fired = animationNameOf(event);
      if (exitNames.length > 0 && fired !== null && !exitNames.includes(fired)) return;
      release();
    };
    node.addEventListener("animationend", done);
    node.addEventListener("animationcancel", done);
    node.addEventListener("transitionend", done);
    // The backstop. A cancelled animation fires `animationcancel` and is caught
    // above; an animation that is REPLACED, or a transition on a property that
    // never actually changes, fires nothing, and only this releases the node.
    timer = setTimeout(release, fallbackMs(runMs));

    return () => {
      // Reopening mid-exit lands here: both the listeners and the pending
      // backstop die before they can unmount a node that is now open again.
      if (timer !== undefined) clearTimeout(timer);
      node.removeEventListener("animationend", done);
      node.removeEventListener("animationcancel", done);
      node.removeEventListener("transitionend", done);
    };
  }, [open]);

  const ref = React.useCallback((element: HTMLElement | null) => {
    nodeRef.current = element;
  }, []);

  return { present, ref, state: open ? "open" : "closed" };
}
