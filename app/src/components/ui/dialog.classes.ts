import type { DialogConfig, DialogState } from "@/components/ui/dialog.behavior";

export interface DialogClassSet {
  overlay: string;
  container: string;
  content: string;
  header: string;
  footer: string;
  title: string;
  description: string;
  close: string;
  closeIcon: string;
}

const overlayClasses = "fixed inset-0 z-depth-overlay bg-foreground/80";

const containerClasses = "fixed inset-0 z-depth-modal flex items-center justify-center p-4";

// data-[state=closed]:pointer-events-none -- the ratified motion ruling's
// taste residue: while a closing overlay is held present through its exit
// window (usePresence defers the unmount), it must not swallow clicks.
// Enter/exit is PRESENCE (#1996): the node MOUNTS with its keyframe attached
// and runs it -- no @starting-style, which is a transitions-on-mount hack this
// system does not use -- and usePresence holds the unmount until the exit
// keyframe ends.
//
// THE CELL IS THE SPEC (#2017). These two utilities are the generated
// consumption of two rows of packages/ui/docs/spec/matrix/motion.jsonl --
// dialog / content / closed -> open (normal, enter, extent pop) and
// dialog / content / open -> closed (moderate, exit, extent pop). One reference
// per cell, exactly as every other generic is consumed: no raw var(), no
// literal, and no shared animation standing in for three distinct moments (the
// #2012 defect this replaces).
//
// NO motion-reduce:animate-none. Reduced motion is handled INSIDE the generated
// utility, which zeroes animation-duration under the media query. Adding
// animate-none here would win destructively -- `animation: none` resets the
// shorthand and discards the zeroed duration with it -- and would also drop the
// element short of the keyframe's end state, which a zeroed duration reaches
// instantly.
const contentClasses =
  "relative w-full max-w-lg rounded-lg border border-card-border bg-card p-6 text-card-foreground shadow-lg " +
  "data-[state=open]:animate-dialog-content-open data-[state=closed]:animate-dialog-content-close " +
  "data-[state=closed]:pointer-events-none";

const headerClasses = "flex flex-col space-y-1.5 text-center @md:text-left";

const footerClasses = "flex flex-col-reverse @md:flex-row @md:justify-end @md:space-x-2";

const titleClasses = "text-title-medium ts-title-medium leading-none";

const descriptionClasses = "text-body-small ts-body-small text-muted-foreground";

const closeClasses =
  "absolute right-2 top-2 inline-flex h-11 w-11 items-center justify-center " +
  "@md:right-4 @md:top-4 @md:h-8 @md:w-8 " +
  "rounded-sm opacity-70 ring-offset-background cursor-pointer " +
  "transition-opacity duration-fast motion-reduce:transition-none hover:opacity-100 " +
  "focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-2";

const closeIconClasses = "h-5 w-5 @md:h-4 @md:w-4";

export function dialogClasses(_config: DialogConfig, _state: DialogState): DialogClassSet {
  return {
    overlay: overlayClasses,
    container: containerClasses,
    content: contentClasses,
    header: headerClasses,
    footer: footerClasses,
    title: titleClasses,
    description: descriptionClasses,
    close: closeClasses,
    closeIcon: closeIconClasses,
  };
}
