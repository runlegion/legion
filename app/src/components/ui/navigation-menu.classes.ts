import type {
  NavigationMenuConfig,
  NavigationMenuState,
} from "@/components/ui/navigation-menu.behavior";

export interface NavigationMenuClassSet {
  root: string;
  list: string;
  item: string;
  trigger: string;
  triggerChevron: string;
  content: string;
  link: string;
  viewportWrapper: string;
  viewport: string;
  indicator: string;
  indicatorArrow: string;
}

const rootClasses = "relative z-10 flex max-w-max flex-1 items-center justify-center";

const listClasses = "group flex flex-1 list-none items-center justify-center gap-1";

// The item is the hover/focus SCOPE for its own panel (#2148): trigger and
// content are siblings inside it, and the panel is absolutely positioned flush
// against the item's bottom edge, so the pointer never crosses a gap travelling
// from trigger to panel -- which matters here, because navigation-menu's close
// carries no linger to forgive a flicker. The named group is a marker, not a
// style: `relative` is still the only declaration the item makes.
const itemClasses = "relative group/navigation-item";

// `motion-hover` is the generated semantic utility for the interactive-surface
// hover cell (color, fast, standard) -- motion.jsonl's navigation-menu /
// trigger / "hover" row, verbatim, instead of a hand-written transition-colors
// pair with a literal duration and a component-level reduced-motion escape.
const triggerClasses =
  "group inline-flex h-11 @md:h-10 w-max items-center justify-center rounded-md " +
  "bg-background px-4 py-2 text-label-medium ts-label-medium cursor-pointer " +
  "motion-hover " +
  "hover:bg-accent hover:text-accent-foreground " +
  "focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-2 " +
  "disabled:opacity-50 disabled:cursor-not-allowed " +
  "data-[state=open]:bg-accent-subtle";

// The chevron turns with the panel it announces, so it takes the panel's own
// item-change tier through `motion-toggle` (transform, moderate, standard).
const triggerChevronClasses = "ml-1 h-3 w-3 motion-toggle group-data-[state=open]:rotate-180";

// MOTION IS CSS AND TOKENS ONLY (#2148). The hover-intent delay that used to be
// a JavaScript timer reading the `hover-intent` delay token through the DOM is
// now `transition-delay` on the reveal rule, and the reveal itself is the item's
// native `:hover` / `:focus-within` -- so the panel opens on a JS-off page.
//
// THE CELL IS THE SPEC. These utilities are the consumption of two rows of
// packages/ui/docs/spec/matrix/motion.jsonl -- navigation-menu / panel /
// "closed -> open" (moderate, enter, delay hover-intent) and navigation-menu /
// panel / "open -> closed" (fast, exit, NO delay). The old
// `createMenuHoverIntent` reused the hover-intent delay for its CLOSE timer as
// well; that was drift against the matrix, and the close is now immediate --
// still animated over the fast/exit cell, just not held back.
//
// The CLOSED cell is the base rule and the OPEN cell is the reveal rule, which
// is how a CSS transition already works: whichever rule currently applies owns
// the duration/curve/delay of the change into it.
//
// The `data-[state=open]` path is the JS-on half: keyboard opening (ArrowDown,
// Enter/Space), a controlled `value`, and a tap on a device with no hover all
// reach the panel through the score rather than through `:hover`. It carries the
// open cell's duration and curve, but NOT the hover-intent delay: that delay
// filters accidental pointer transit, and there is no accidental ArrowDown. The
// hover path keeps its delay regardless, because `group-hover` is the rule that
// declares it and nothing in the data-state group competes for transition-delay.
//
// `data-dismissed` is the WCAG 1.4.13 dismissal flag, raised by an Escape
// keydown (or by a click that closed the panel) and cleared by a deliberate
// reopen or by the pointer/focus leaving the menu. It is raised on THE DISMISSED
// PANEL, never on the root: a root-scoped force-hide blanked every panel in the
// bar at once, so after Escape a hover onto a SIBLING trigger revealed nothing
// until the pointer left the whole nav -- a dead zone the retired hover-intent
// never had. It has to win against reveal rules that are one class-level more
// specific, hence the important form: after Escape the focus returns to the
// trigger, so `:focus-within` still matches and only a force-hide can put the
// panel back down.
//
// `pointer-events` rides the transition (`transition-[opacity,pointer-events]` +
// `transition-discrete`) instead of being switched by the reveal rule, the same
// way tooltip's and hover-card's do and for the same reason: a rule that owns
// both makes the panel click-through the instant `:hover` drops, while it is
// still most of the way opaque. On the transition it flips only once the fade
// passes its halfway point, so what is visible is what is clickable. The reveal
// rules re-state `transition-property: opacity` so the flip back to `auto` on
// OPEN is immediate rather than half a fade late.
//
// NO `hidden` on the panel and no component-level reduced-motion escape.
// `hidden` is UA-stylesheet `display: none` -- it pulls the panel out of the
// accessibility tree and out of rendering, which both breaks the aria-labelledby
// announcement and kills the transition. Reduced motion is the token sheet's own
// responsibility (the exporter's REDUCED_MOTION_ZEROED set), never a
// component-level media query.
const contentClasses =
  "absolute left-0 top-full w-max rounded-md border bg-popover p-2 text-popover-foreground shadow-lg " +
  "opacity-0 pointer-events-none transition-[opacity,pointer-events] transition-discrete " +
  "duration-fast ease-exit " +
  "group-hover/navigation-item:opacity-100 group-hover/navigation-item:pointer-events-auto " +
  "group-hover/navigation-item:transition-opacity " +
  "group-hover/navigation-item:duration-moderate group-hover/navigation-item:ease-enter " +
  "group-hover/navigation-item:delay-hover-intent " +
  "group-focus-within/navigation-item:opacity-100 " +
  "group-focus-within/navigation-item:pointer-events-auto " +
  "group-focus-within/navigation-item:transition-opacity " +
  "group-focus-within/navigation-item:duration-moderate " +
  "group-focus-within/navigation-item:ease-enter " +
  "group-focus-within/navigation-item:delay-hover-intent " +
  "data-[state=open]:opacity-100 data-[state=open]:pointer-events-auto " +
  "data-[state=open]:transition-opacity " +
  "data-[state=open]:duration-moderate data-[state=open]:ease-enter " +
  "data-[dismissed=true]:opacity-0! " +
  "data-[dismissed=true]:pointer-events-none!";

const linkClasses =
  "block select-none space-y-1 rounded-md p-3 no-underline outline-none " +
  "motion-hover " +
  "hover:bg-accent hover:text-accent-foreground " +
  "active:bg-muted active:text-foreground " +
  "focus-visible:bg-accent focus-visible:text-accent-foreground " +
  "focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-2 " +
  "data-[active]:bg-accent-subtle";

const viewportWrapperClasses = "absolute left-0 top-full";

const viewportClasses =
  "h-min w-full overflow-hidden rounded-md border bg-popover text-popover-foreground shadow-lg";

// The indicator slides between items as the open panel changes, which is
// motion.jsonl's navigation-menu / panel / "open -> open (item change)" cell
// (moderate, standard) -- `motion-toggle`, the generated utility for exactly
// that tier and curve.
const indicatorClasses =
  "absolute bottom-0 z-10 flex h-2.5 items-end justify-center overflow-hidden motion-toggle";

const indicatorArrowClasses = "top-full h-2 w-2 rotate-45 rounded-tl-sm bg-border shadow-md";

export function navigationMenuClasses(
  _config: NavigationMenuConfig,
  _state: NavigationMenuState,
): NavigationMenuClassSet {
  return {
    root: rootClasses,
    list: listClasses,
    item: itemClasses,
    trigger: triggerClasses,
    triggerChevron: triggerChevronClasses,
    content: contentClasses,
    link: linkClasses,
    viewportWrapper: viewportWrapperClasses,
    viewport: viewportClasses,
    indicator: indicatorClasses,
    indicatorArrow: indicatorArrowClasses,
  };
}

/** shadcn-compatible export: the trigger's class string, for consumers who
 *  style a NavigationMenuLink as a trigger. */
export function navigationMenuTriggerStyle(): string {
  return triggerClasses;
}
