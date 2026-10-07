import type { TabsConfig, TabsState } from '@/components/ui/tabs.behavior';

export interface TabsClassSet {
  /** The wrapper: stacks the list above its panels (or beside, when vertical). */
  root: string;
  /** The [role="tablist"] rail. */
  list: string;
  /** Each [role="tab"] trigger, including its data-state styling. */
  trigger: string;
  /** Each [role="tabpanel"]: focusable, so it carries a focus ring. */
  panel: string;
}

const rootHorizontalClasses = 'flex flex-col gap-2';
const rootVerticalClasses = 'flex flex-row gap-2';

const listBaseClasses =
  'inline-flex items-center justify-center rounded-md bg-muted p-1 text-muted-foreground';
const listHorizontalClasses = 'h-10';
const listVerticalClasses = 'h-auto flex-col';

const triggerBaseClasses = [
  'inline-flex items-center justify-center whitespace-nowrap rounded-sm px-3 py-1.5',
  'text-label-medium ts-label-medium ring-offset-background cursor-pointer',
  // motion.jsonl `tabs / trigger / hover` -- color (background, text, border),
  // fast, standard. This is the trigger's ONLY row; the comment that used to sit
  // here called the same declaration "indicator-move", which is a different row
  // and a part this component does not have (see panelClasses below).
  //
  // `transition-all` narrowed to `transition-colors`: it also transitioned the
  // active pill's `shadow-sm`, and the matrix assigns tabs no `elevation` row.
  // A moment with no row does not move.
  'transition-colors duration-fast ease-standard',
  'focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-2',
  'disabled:opacity-50 disabled:cursor-not-allowed disabled:pointer-events-none',
].join(' ');

// State-driven styling: the score projects data-state onto every trigger, so
// one class string serves all three performances with no conditional applying.
const triggerStateClasses = [
  'data-[state=active]:bg-background data-[state=active]:text-foreground data-[state=active]:shadow-sm',
  'data-[state=inactive]:text-muted-foreground hover:bg-muted hover:text-foreground',
].join(' ');

// motion.jsonl `tabs / panel / active change` -- fade (crossfade), fast,
// standard. A presence change, so it is a keyframe: the generator emits this
// cell as `tabs-panel-active-change` (fade-in, fast, standard), which
// deduplicates to the `animate-fade-in-fast-standard` utility.
//
// Unconditional rather than state-keyed, and that is the mechanism: inactive
// panels carry `hidden`, so only the active panel has a box. Clearing `hidden`
// gives the element a box again and the browser restarts its animation -- the
// crossfade-in lands on exactly the panel becoming active. There is no
// crossfade-OUT half: the outgoing panel is `hidden` in the same frame, and the
// matrix assigns tabs no panel-exit row.
//
// NO ROW for the panel's focus ring; it stays instant.
const panelClasses = [
  'ring-offset-background',
  'animate-fade-in-fast-standard',
  'focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-2',
].join(' ');

// NO MOMENT for motion.jsonl `tabs / indicator / active change` (travel (x,
// offset between triggers), fast*, standard*, extent structural (distance)).
// This component has no `indicator` part: the class set is root / list /
// trigger / panel, and the active pill is the trigger's own
// `data-[state=active]:bg-background`, which appears and disappears in place
// rather than travelling along the rail. Faking the row on the trigger is what
// the retired comment above did. Reported on #2307 rather than resolved here --
// giving tabs a real travelling indicator is a component change, not a class
// change.

/** The view: class strings keyed by config/state. No logic. */
export function tabsClasses(config: TabsConfig, _state: TabsState): TabsClassSet {
  const vertical = config.orientation === 'vertical';
  return {
    root: vertical ? rootVerticalClasses : rootHorizontalClasses,
    list: `${listBaseClasses} ${vertical ? listVerticalClasses : listHorizontalClasses}`,
    trigger: `${triggerBaseClasses} ${triggerStateClasses}`,
    panel: panelClasses,
  };
}
