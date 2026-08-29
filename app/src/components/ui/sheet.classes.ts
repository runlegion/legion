import type { SheetConfig, SheetSide, SheetState } from "@/components/ui/sheet.behavior";

export interface SheetClassSet {
  overlay: string;
  header: string;
  footer: string;
  title: string;
  description: string;
  close: string;
  closeIcon: string;
}

const overlayClasses = "fixed inset-0 z-depth-overlay bg-foreground/80";

// Base content signature shared across sides. The per-side anchor, size and
// border edge come from `sheetSideClasses`. No animation utilities and no raw
// durations: the semantic slide-per-side motion tokens do not exist yet (the
// motion token layer is being rebuilt, #1899), so motion is left UNDECLARED
// rather than hardcoded. Enter-only ships once those tokens land (Presence,
// wave 0-B). data-state is projected so a future token layer can hook it.
const contentBaseClasses =
  "fixed z-depth-modal flex flex-col gap-4 bg-background p-6 shadow-lg " +
  "data-[state=closed]:pointer-events-none";

// Per-side placement: one axis, one edge. Left/right run full height and cap
// their width through a container query (the CQ system rule -- viewport
// breakpoints moved to container context, cf. dialog); top/bottom span the
// inline axis and hug the block edge.
const sheetSideClasses: Record<SheetSide, string> = {
  top: "inset-x-0 top-0 border-b border-card-border",
  bottom: "inset-x-0 bottom-0 border-t border-card-border",
  left: "inset-y-0 left-0 h-full w-3/4 @sm:max-w-sm border-r border-card-border",
  right: "inset-y-0 right-0 h-full w-3/4 @sm:max-w-sm border-l border-card-border",
};

const headerClasses = "flex flex-col space-y-2 text-center @md:text-left";

const footerClasses = "flex flex-col-reverse @md:flex-row @md:justify-end @md:space-x-2";

const titleClasses = "text-title-medium ts-title-medium leading-none text-foreground";

const descriptionClasses = "text-body-small ts-body-small text-muted-foreground";

const closeClasses =
  "absolute right-2 top-2 inline-flex h-11 w-11 items-center justify-center " +
  "@md:right-4 @md:top-4 @md:h-8 @md:w-8 " +
  "rounded-sm opacity-70 ring-offset-background cursor-pointer " +
  "transition-opacity duration-150 motion-reduce:transition-none hover:opacity-100 " +
  "focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-2";

const closeIconClasses = "h-5 w-5 @md:h-4 @md:w-4";

/** The content class: the side-independent base signature plus the anchored
 *  placement for `side`. The ONLY source of the content class -- every
 *  performance calls this, so the side (a Content-level decoration prop
 *  mirroring shadcn) is the single argument the base does not already fix. */
export function sheetContentClasses(side: SheetSide): string {
  return `${contentBaseClasses} ${sheetSideClasses[side]}`;
}

/** The side-independent class set. The content class is NOT here: it depends on
 *  `side`, so it is produced by `sheetContentClasses(side)` by whoever knows the
 *  side. Everything below is invariant across sides and open state. */
export function sheetClasses(_config: SheetConfig, _state: SheetState): SheetClassSet {
  return {
    overlay: overlayClasses,
    header: headerClasses,
    footer: footerClasses,
    title: titleClasses,
    description: descriptionClasses,
    close: closeClasses,
    closeIcon: closeIconClasses,
  };
}

export { sheetSideClasses };
