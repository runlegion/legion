import { createBehavior, type AriaAttrs, type BehaviorSpec } from '@/lib/contract';
import { updateAriaAttribute } from '@/lib/primitives/aria-manager';
import { createRovingFocus } from '@/lib/primitives/roving-focus';

/**
 * Grid: named attention structures over a 12-column vocabulary. A static
 * score -- no state, no actions, no keymap -- but the structure contract
 * (roles, per-instance priority projection, the 2D roving keyboard
 * navigation) is behavior, and the harness audits it here.
 *
 * Ruled 2026-07-03:
 * - columns are whatever the agent wants, 1-12 (the literal class ceiling
 *   and the span denominator); Miller's-law limits are advice in the
 *   intelligence layer, never enforcement.
 * - item priority is 100% behavior: items DECLARE what they are
 *   (data-priority projection), decoration places them. The oracle's
 *   [&>*:first-child] positional magic is defect-do-not-port.
 * - role="grid" is honest or absent: fixed columns only (type-gated in the
 *   performance), row/gridcell structure, 2D roving keyboard.
 */

export type GridPreset = 'linear' | 'golden' | 'bento';
export type BentoPattern = 'editorial' | 'dashboard' | 'feature' | 'portfolio';
export type ContentPriority = 'primary' | 'secondary' | 'tertiary';

export type ColumnsValue = 1 | 2 | 3 | 4 | 5 | 6 | 7 | 8 | 9 | 10 | 11 | 12 | 'auto';

export interface ResponsiveColumnsObject {
  base?: ColumnsValue | undefined;
  sm?: ColumnsValue | undefined;
  md?: ColumnsValue | undefined;
  lg?: ColumnsValue | undefined;
  xl?: ColumnsValue | undefined;
  '2xl'?: ColumnsValue | undefined;
}

export type ResponsiveColumns = ColumnsValue | ResponsiveColumnsObject;

export type SpacingValue = '0' | '1' | '2' | '3' | '4' | '5' | '6' | '8' | '10' | '12';

export interface GridConfig {
  preset?: GridPreset | undefined;
  pattern?: BentoPattern | undefined;
  columns?: ResponsiveColumns | undefined;
  gap?: SpacingValue | undefined;
  padding?: SpacingValue | undefined;
  /** 'grid' promises the ARIA grid pattern: fixed columns, row/gridcell
   *  structure, arrow-key navigation. The performance type-gates it. */
  role?: 'presentation' | 'grid' | undefined;
  /** Accessible name; required with role='grid' (type-gated). */
  ariaLabel?: string | undefined;
}

export type GridState = Record<never, never>;
export type GridActions = Record<never, never>;
export type GridPart = 'root' | 'row' | 'cell';

function fixedColumns(config: GridConfig): number | null {
  return typeof config.columns === 'number' ? config.columns : null;
}

export const grid: BehaviorSpec<GridConfig, GridState, GridActions, GridPart> = {
  name: 'grid',
  parts: {
    root: {},
    // Present only in role='grid' mode; the harness asserts their roles.
    row: { role: 'row', many: true, optional: true },
    cell: { role: 'gridcell', many: true, optional: true },
  },
  initialState: () => ({}),
  actions: {},
  canDispatch: () => true,
  aria: (_state, config) => ({
    root: {
      role: config.role === 'grid' ? 'grid' : undefined,
      'aria-label': config.role === 'grid' ? config.ariaLabel : undefined,
      'data-preset': config.preset ?? 'linear',
      // The projection OWNS data-preset/data-columns: the markup must not also
      // emit them, or re-applying the projection would fight the authored
      // value. `auto` is projected too so the attribute round-trips through
      // readGridConfig for every attribute-expressible column value.
      'data-columns':
        typeof config.columns === 'number' || config.columns === 'auto'
          ? String(config.columns)
          : undefined,
    },
  }),
  // Static score, like container: the ARIA grid keyboard contract is not a
  // behavior action. The bindings compose the roving-focus primitive directly
  // (createRovingFocus with its 2D columns option), gated on an honest
  // role="grid" with a fixed column count -- see bindGrid below and grid.tsx.
  keymap: () => null,
};

/** Per-instance projection for grid items: the item DECLARES its priority;
 *  the stock layouts in grid.classes.ts key placement off the projected
 *  attribute, never off source position. */
export function gridItemAttrs(priority: ContentPriority | undefined): AriaAttrs {
  return { 'data-priority': priority };
}

/** Parse the WC/Astro `data-columns` attribute into the score's config value.
 *  Only a bare integer or `auto` is expressible as an attribute; a
 *  responsive object is a React-only prop. A non-numeric, non-`auto` value
 *  reads as absent (auto default). */
function parseColumns(raw: string | undefined): ResponsiveColumns | undefined {
  if (raw === undefined) return undefined;
  if (raw === 'auto') return 'auto';
  if (/^\d+$/.test(raw)) return Number(raw) as ColumnsValue;
  return undefined;
}

/**
 * Reconstruct the score's config from a root element's `data-*` attributes --
 * the inverse of the SSR/WC markup, and the pairing #2001 asks for: config
 * travels as `data-*` ONLY, so the read is `element.dataset` rather than a
 * hand-rolled getAttribute over invented attribute names.
 *
 * `data-preset` and `data-columns` are written by the score's own aria
 * projection (see grid.aria above), so they are already on the root before
 * this ever runs; the remaining keys are authored in the markup.
 */
export function readGridConfig(root: HTMLElement): GridConfig {
  const data = root.dataset;
  return {
    preset: data['preset'] as GridPreset | undefined,
    pattern: data['pattern'] as BentoPattern | undefined,
    columns: parseColumns(data['columns']),
    gap: data['gap'] as SpacingValue | undefined,
    padding: data['padding'] as SpacingValue | undefined,
    role: data['gridRole'] === 'grid' ? 'grid' : undefined,
    ariaLabel: root.getAttribute('aria-label') ?? undefined,
  };
}

/**
 * The DOM-native binding of the grid score -- the client. The Web Component
 * and the Astro <script> both import THIS; only React (retained-mode) reads
 * the projection declaratively. Grid is a STATIC score, so the binding is the
 * thinnest of the family: no click/keydown wiring (grid has no keymap and no
 * actions), just the one-shot projection apply and the roving-focus primitive,
 * composed directly and engaged only under an honest role="grid".
 *
 * Three-gotcha ledger for this archetype:
 *   1. Controlled-callback before/after: N/A. Grid has no controlled value
 *      and no actions (GridActions = Record<never, never>), so there is no
 *      callback to fire and nothing to compare.
 *   2. aria-manager coerces the resolved string 'false' to truthy -- the
 *      projection is already final, so apply it with { validate: false }
 *      (below), skipping author-input coercion.
 *   3. WC bind deferred one microtask -- see grid.element.ts.
 *
 * The role disposition (carried from the WC port): a bare `role="grid"` on a
 * light-DOM host BEFORE the row/gridcell children exist is a 4.1.2 axe
 * violation and collides with the platform `role`. So the opt-in attribute is
 * `data-grid-role`; the binding reads it and PROJECTS the real `role="grid"` onto
 * the root part, which by then owns authored row/gridcell descendants.
 */
export function bindGrid(root: HTMLElement): () => void {
  const config: GridConfig = readGridConfig(root);

  const { memory } = createBehavior(grid, config);

  // role="grid" with a fixed column count engages the ARIA grid keyboard
  // contract: 2D roving across the [data-roving-item] cells. Composed
  // directly -- createRovingFocus owns the roving tabindex and arrow/Home/End
  // movement (Left/Right by 1, Up/Down by a row). Presentation grids and
  // fluid-column grids stay inert: the honesty gate is the same predicate the
  // score's aria projection uses (fixed columns only).
  const columns = fixedColumns(config);
  const stopRoving =
    config.role === 'grid' && columns !== null ? createRovingFocus(root, { columns }) : undefined;

  // The projection is already resolved (final strings, undefined = absent),
  // so apply it raw: validate:false skips aria-manager's author-input
  // coercion, which would re-interpret a string like 'false' as truthy.
  const applyProjection = (el: HTMLElement, attrs: AriaAttrs) => {
    for (const [name, value] of Object.entries(attrs)) {
      updateAriaAttribute(el, name as never, value as never, { validate: false });
    }
  };

  const render = () => {
    const state = memory.get();
    const projection = grid.aria(state, config, { root: root.id ?? '', row: '', cell: '' });
    if (projection.root) applyProjection(root, projection.root);
  };
  const unsubscribe = memory.subscribe(render); // fires immediately: first paint

  return () => {
    unsubscribe();
    stopRoving?.();
  };
}
