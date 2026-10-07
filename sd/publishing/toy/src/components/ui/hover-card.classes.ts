import type { HoverCardConfig, HoverCardState } from '@/components/ui/hover-card.behavior';

export interface HoverCardClassSet {
  trigger: string;
  content: string;
}

// The DOM-native root is a binding host, not a box: it carries data-part="root"
// and the config, and NO class -- a behavior root never styles itself; layout
// belongs to the consumer's Container/Grid (operator ruling, 2026-08-02). With
// no class the unclassed <div> root is a BLOCK box, so a hover-card is composed
// by Container like any other component -- it is not dropped mid-sentence into
// running text.

// Inside that root the trigger is still an inline-flex anchor: the consumer's
// link keeps its own type, underline, and focus affordances, and hover-card
// adds no chrome to it.
const triggerClasses = 'inline-flex';

// MOTION IS CSS AND TOKENS ONLY (#2148). No TypeScript reads a motion token, no
// timer implements a delay, and the whole reveal works with JavaScript turned
// off. The two hardcoded delay literals the behaviour used to carry are gone;
// the reveal is native `:hover` / `:focus-visible` over the root's real sibling
// structure (root > trigger, root > content) and the timing is
// `transition-delay` on the token custom properties.
//
// THE CELL IS THE SPEC. These utilities are the consumption of two rows of
// packages/ui/docs/spec/matrix/motion.jsonl -- hover-card / content /
// "closed -> open" (moderate, enter, delay hover-intent) and hover-card /
// content / "open -> closed" (fast, exit, delay LINGER). Hover-card is the ONE
// component of the three hover-triggered surfaces whose close carries a delay:
// the pointer is expected to travel from a small inline anchor onto a wide
// preview panel, and the linger is what forgives the near-miss on the way. The
// matrix says so; tooltip and navigation-menu close with no delay at all.
//
// The CLOSED cell is the base rule and the OPEN cell is the reveal rule, which
// is how a CSS transition already works: whichever rule currently applies owns
// the duration/curve/delay of the change into it.
//
// The `data-[state=open]:animate-in fade-in-0 zoom-in-95 slide-in-from-*`
// vocabulary that used to sit here is gone with the same reasoning popover's
// classes record: it is the tailwindcss-animate plugin's, which this repo does
// not ship, and it was keyed off a `data-state` nothing writes without JS. The
// matrix's `properties` for this cell are opacity and transform: scale; the
// directional slide was never in it.
//
// `fixed` is the base positioning, not decoration: the content is now rendered
// unconditionally in every performance (no `hidden`, no conditional null), so a
// flow-positioned panel would reserve layout space next to every trigger on a
// JS-off page. The collision-detector's inline `position: fixed; left: 0;
// top: 0; transform: ...` overrides it once JS runs.
//
// NO component-level reduced-motion escape. The generated `duration-*` and
// `delay-*` utilities zero themselves under prefers-reduced-motion (the
// exporter's REDUCED_MOTION_ZEROED set,
// packages/design-tokens/src/exporters/tailwind.ts), so reduced motion is the
// token sheet's responsibility and never a component-level media query.
//
// `data-disable-hoverable-content` is read by the CSS directly, never by JS.
// `data-dismissed` is the WCAG 1.4.13 dismiss escape hatch, set by an Escape
// keydown and cleared on pointerleave/blur; it has to win against a reveal rule
// one class-level more specific, hence the important form.
//
// THE REVEAL RULE MUST NOT OWN `pointer-events` -- doubly so here. A rule that
// switches hit-testing on `:hover` makes the panel click-through for its whole
// linger: fully opaque, fully transparent to the pointer, for 300ms. That
// destroys the very thing the linger exists for, which is forgiving the pointer
// its near-miss on the way from a small inline anchor to a wide preview panel;
// a panel it cannot re-enter is not forgiven anything. So pointer-events rides
// the transition instead (`transition-[opacity,pointer-events]` +
// `transition-discrete`) and flips only once the fade passes its halfway point
// -- measured at ~413ms into hover-card's 300ms linger + 150ms fade in Chromium,
// Firefox, and WebKit alike, so the panel stays hit-testable for the entire
// linger and the resting panel is still inert. The reveal rule re-states
// `transition-property: opacity` so the flip back to `auto` on OPEN is
// immediate. Browsers without `transition-behavior` do not transition a
// discrete property at all and land back on the immediate flip -- degraded,
// never broken.
//
// The close delay is scoped `:not([data-state=open])` rather than stated
// unconditionally, and that scope is load-bearing: `delay-linger` on the naked
// base rule is the delay a CONTROLLED open would inherit, since the data-state
// path deliberately carries no delay of its own (hover-intent filters accidental
// pointer transit; a consumer forcing `open` true has declared intent, and the
// issue says the data-state path reveals "immediately (no delay)"). Unscoped,
// dropping hover-intent from that path would have handed a forced-open card the
// 300ms LINGER instead -- slower than what it replaced. The hover path is
// unaffected either way: its reveal selector computes at (0,4,0) against this
// rule's (0,2,0).
const contentClasses =
  'fixed z-depth-popover w-64 rounded-md border bg-popover p-4 text-popover-foreground ' +
  'shadow-lg outline-none ' +
  'opacity-0 pointer-events-none transition-[opacity,pointer-events] transition-discrete ' +
  'duration-fast ease-exit [&:not([data-state=open])]:delay-linger ' +
  '[:is([data-hover-card]:has(>[data-part=trigger]:is(:hover,:focus-visible)),[data-hover-card]:not([data-disable-hoverable-content=true]):hover)>&]:opacity-100 ' +
  '[:is([data-hover-card]:has(>[data-part=trigger]:is(:hover,:focus-visible)),[data-hover-card]:not([data-disable-hoverable-content=true]):hover)>&]:pointer-events-auto ' +
  '[:is([data-hover-card]:has(>[data-part=trigger]:is(:hover,:focus-visible)),[data-hover-card]:not([data-disable-hoverable-content=true]):hover)>&]:transition-opacity ' +
  '[:is([data-hover-card]:has(>[data-part=trigger]:is(:hover,:focus-visible)),[data-hover-card]:not([data-disable-hoverable-content=true]):hover)>&]:duration-moderate ' +
  '[:is([data-hover-card]:has(>[data-part=trigger]:is(:hover,:focus-visible)),[data-hover-card]:not([data-disable-hoverable-content=true]):hover)>&]:ease-enter ' +
  '[:is([data-hover-card]:has(>[data-part=trigger]:is(:hover,:focus-visible)),[data-hover-card]:not([data-disable-hoverable-content=true]):hover)>&]:delay-hover-intent ' +
  'data-[state=open]:opacity-100 data-[state=open]:pointer-events-auto ' +
  'data-[state=open]:transition-opacity ' +
  'data-[state=open]:duration-moderate data-[state=open]:ease-enter ' +
  '[[data-hover-card][data-dismissed=true]>&]:opacity-0! ' +
  '[[data-hover-card][data-dismissed=true]>&]:pointer-events-none!';

export function hoverCardClasses(
  _config: HoverCardConfig,
  _state: HoverCardState,
): HoverCardClassSet {
  return {
    trigger: triggerClasses,
    content: contentClasses,
  };
}
