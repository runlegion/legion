import type { AriaAttrs, BehaviorSpec } from '@/lib/contract';

/**
 * Item: a generic list row. Leading (icon) / content (label + description) /
 * trailing layout, styled as a single `role="option"` row. A static score --
 * NO state, NO actions, NO keymap, NO effects -- but, unlike Card and
 * Container, the projection is NOT empty: `role="option"` and the selected /
 * disabled semantics are the item's contract, not native to a `div`, so the
 * score PROJECTS them from config. `aria-selected` and `aria-disabled` (plus
 * the `data-selected`/`data-disabled` styling hooks and the `tabindex`) are
 * computed once, here, and every framework applies the same projection.
 *
 * Because the projection is derived purely from config (there is no runtime
 * state to remember and nothing to observe), Item needs NO client: there is
 * no `bindItem`, the React performance reads the projection synchronously in
 * render (no `useMemory`), the Astro performance ships no `<script>`, and the
 * Web Component applies the projection once in `render()`. This is the
 * card/container no-bind shape carrying a config-driven projection: the score
 * is a total function from config to attributes, so the three performances
 * cannot drift.
 *
 * The old row wired Enter/Space to a synthetic click so a bare `div` could be
 * activated. That is dropped here (see the doc's oracle table): a list row is
 * an OPTION owned by a listbox/menu parent, and the parent owns roving focus
 * and activation. Item projects the option semantics; it does not fabricate a
 * control's keyboard contract.
 */

export type ItemSize = 'default' | 'sm' | 'lg';

const ALLOWED_SIZES: ReadonlyArray<ItemSize> = ['default', 'sm', 'lg'];

export interface ItemConfig {
  /** Visual size variant. */
  size?: ItemSize | undefined;
  /** Selected/active state -- projects aria-selected + data-selected. */
  selected?: boolean | undefined;
  /** Disabled state -- projects aria-disabled + data-disabled, tabindex -1. */
  disabled?: boolean | undefined;
}

export type ItemState = Record<never, never>;
export type ItemActions = Record<never, never>;
export type ItemPart = 'root';

/** Coerce an untyped attribute/prop into a known size, defaulting safely.
 *  Shared so the WC and Astro fall back identically to the React default. */
export function parseItemSize(value: string | null | undefined): ItemSize {
  if (value && (ALLOWED_SIZES as ReadonlyArray<string>).includes(value)) {
    return value as ItemSize;
  }
  return 'default';
}

/**
 * The root projection, resolved from config. `role="option"` is unconditional
 * (an item is always a selectable row); `aria-selected` is ALWAYS present
 * (an option carries its selection state either way); `aria-disabled`,
 * `data-selected`, `data-disabled` are present only when true (undefined = the
 * attribute must not render); `tabindex` is `-1` when disabled so the row
 * leaves the tab order but a parent listbox can still reach it, `0` otherwise.
 */
function itemRootAria(config: ItemConfig): AriaAttrs {
  const selected = config.selected ?? false;
  const disabled = config.disabled ?? false;
  return {
    role: 'option',
    'aria-selected': selected ? 'true' : 'false',
    'aria-disabled': disabled ? 'true' : undefined,
    tabindex: disabled ? '-1' : '0',
    'data-selected': selected ? '' : undefined,
    'data-disabled': disabled ? '' : undefined,
  };
}

export const item: BehaviorSpec<ItemConfig, ItemState, ItemActions, ItemPart> = {
  name: 'item',
  parts: { root: {} },
  initialState: () => ({}),
  actions: {},
  canDispatch: () => true,
  // The one live contract: option semantics projected from config. Everything
  // else (state/keymap) is empty -- there is nothing to bind.
  aria: (_state, config) => ({ root: itemRootAria(config) }),
  keymap: () => null,
};
