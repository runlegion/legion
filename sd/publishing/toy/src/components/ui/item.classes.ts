import type { ItemConfig, ItemSize, ItemState } from '@/components/ui/item.behavior';
import { parseItemSize } from '@/components/ui/item.behavior';

export interface ItemClassSet {
  root: string;
}

/**
 * The row's structure and its state visuals. State is styled by VARIANT, not
 * by a JS branch: the score projects `aria-selected` / `aria-disabled` and the
 * `aria-*` Tailwind variants react to them (attribute-selector specificity
 * wins over the base `text-foreground` regardless of compiled source order).
 * Selected and hover both resolve to the accent pairing -- same tokens, so
 * they never compete -- and `aria-disabled:pointer-events-none` suppresses
 * hover on a disabled row. The old imperative `stateStyles` branch is replaced
 * by these variants: one projection, one declarative style rule per state.
 */
const itemBaseClasses =
  'flex items-center gap-3 rounded-md cursor-default select-none outline-none ' +
  'text-foreground hover:bg-accent hover:text-accent-foreground ' +
  'aria-selected:bg-accent aria-selected:text-accent-foreground ' +
  'aria-disabled:opacity-50 aria-disabled:pointer-events-none aria-disabled:text-muted-foreground';

/** Keyboard-focus affordance -- focus-visible only, so pointer clicks stay quiet. */
const itemFocusClasses =
  'focus-visible:bg-accent focus-visible:text-accent-foreground ' +
  'focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-1';

/**
 * `item / root / hover` in `packages/ui/docs/spec/matrix/motion.jsonl` assigns
 * tier `fast` and curve role `standard` (provenance `baseline`) to a colour
 * change (`background, text, border`) on a part that stays put -- a
 * TRANSITION, named as composed generics. `ease-standard` was the half of the
 * row this file had not yet named; the timing carries no literal.
 *
 * NO component-level reduced-motion escape. The generated `duration-*` and
 * `delay-*` utilities zero themselves under `prefers-reduced-motion` (the
 * exporter's `REDUCED_MOTION_ZEROED` set), so reduced motion is the token
 * sheet's responsibility and never a component-level media query.
 */
const itemMotionClasses = 'transition-colors duration-fast ease-standard';

/** Size variants: padding + the semantic typography role token per scale. */
export const itemSizeClasses: Record<ItemSize, string> = {
  default: 'px-3 py-2 text-body-small ts-body-small',
  sm: 'px-2 py-1.5 text-label-small ts-label-small',
  lg: 'px-4 py-3 text-body-medium ts-body-medium',
};

/**
 * Sub-part classes are config-independent literals, so the framework files
 * import them directly (a flat static needs no context/provider). Ported from
 * the oracle's settled composition.
 */
export const itemIconClasses = 'shrink-0 text-current';

export const itemContentClasses = 'flex min-w-0 flex-1 flex-col';

export const itemLabelClasses = 'truncate';

export const itemDescriptionClasses =
  'truncate text-muted-foreground text-label-small ts-label-small mt-0.5';

/**
 * The row class string. Only `size` varies with config; the state visuals ride
 * the projected `aria-*` attributes, so they live in the static base. One
 * projection, three performances, identical composition.
 */
export function itemClasses(config: ItemConfig, _state: ItemState): ItemClassSet {
  const size = parseItemSize(config.size);
  return {
    root: [itemBaseClasses, itemSizeClasses[size], itemFocusClasses, itemMotionClasses].join(' '),
  };
}
