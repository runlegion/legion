/**
 * <rafters-grid> Web Component
 *
 * Framework-target for the Grid component, parallel to grid.tsx (React) and
 * grid.astro (Astro). The inner div carries the SAME utility class strings the
 * React/Astro targets use -- imported from grid.classes.ts -- rather than a
 * parallel hand-written CSS map. Presentation resolves from the shared compiled
 * utility sheet adopted by RaftersElement (setUtilityCSS) plus the token custom
 * properties inherited from the host :root.
 *
 * The shadow-scoped CSS this component owns is the structural :host shim, which
 * declares container-type so the grid responds to its parent container's inline
 * size. That declaration has no inner-element utility-class equivalent and so is
 * preserved verbatim in static styles.
 *
 * Shadow DOM structure: an inner div carrying the composed grid utility classes,
 * wrapping a default slot.
 *
 * Attributes:
 *   cols  1 | 2 | 3 | 4 | 6 | 12          (default 1)
 *   gap   0 | 1 | 2 | 3 | 4 | 5 | 6 | 8 | 10 | 12  (default 4)
 *   flow  row | col | dense                (default row)
 *
 * Unknown attribute values fall back to defaults silently.
 *
 * @cognitive-load 4/10
 * @accessibility Layout-only container; consumers may set role on the host
 *                element when interactive grid semantics are required.
 */

import { RaftersElement } from '@/lib/primitives/rafters-element';
import { gridColumnClasses, gridGapClasses } from '@/components/ui/grid.classes';

export type GridCols = 1 | 2 | 3 | 4 | 6 | 12;
export type GridGap = 0 | 1 | 2 | 3 | 4 | 5 | 6 | 8 | 10 | 12;
export type GridFlow = 'row' | 'col' | 'dense';

export const GRID_COLS_VALUES: readonly GridCols[] = [1, 2, 3, 4, 6, 12];
export const GRID_GAP_VALUES: readonly GridGap[] = [0, 1, 2, 3, 4, 5, 6, 8, 10, 12];
export const GRID_FLOW_VALUES: readonly GridFlow[] = ['row', 'col', 'dense'];

export const DEFAULT_GRID_COLS: GridCols = 1;
export const DEFAULT_GRID_GAP: GridGap = 4;
export const DEFAULT_GRID_FLOW: GridFlow = 'row';

/**
 * Grid auto-flow utility classes. Tailwind's grid-flow utilities; `dense`
 * maps to row-dense flow, matching the React/Astro flow semantics.
 */
export const gridFlowClasses: Record<GridFlow, string> = {
  row: 'grid-flow-row',
  col: 'grid-flow-col',
  dense: 'grid-flow-row-dense',
};

export const gridBaseClasses = 'grid';

function parseEnumInt<T extends number>(raw: string | null, allowed: readonly T[], fallback: T): T {
  if (raw === null) return fallback;
  const parsed = Number.parseInt(raw, 10);
  if (Number.isNaN(parsed)) return fallback;
  for (const candidate of allowed) {
    if (candidate === parsed) return candidate;
  }
  return fallback;
}

function parseEnumString<T extends string>(
  raw: string | null,
  allowed: readonly T[],
  fallback: T,
): T {
  if (raw === null) return fallback;
  for (const candidate of allowed) {
    if (candidate === raw) return candidate;
  }
  return fallback;
}

/**
 * Compose the inner div's class string from the shared class maps plus the
 * local flow map. Exported so tests assert the WC renders the exact same
 * composition the Astro target does -- the parity guarantee.
 */
export function composeGridClasses(cols: GridCols, gap: GridGap, flow: GridFlow): string {
  return `${gridBaseClasses} ${gridColumnClasses[cols]} ${gridGapClasses[String(gap)]} ${gridFlowClasses[flow]}`;
}

export class RaftersGrid extends RaftersElement {
  /**
   * The only component-owned CSS: the structural host shim. container-type
   * makes the grid respond to its parent container's inline size; it has no
   * utility-class equivalent on the inner div, so it lives here.
   */
  static override styles = ':host { display: block; container-type: inline-size; }';

  static readonly observedAttributes: ReadonlyArray<string> = ['cols', 'gap', 'flow'];

  override render(): Node {
    const cols = parseEnumInt(this.getAttribute('cols'), GRID_COLS_VALUES, DEFAULT_GRID_COLS);
    const gap = parseEnumInt(this.getAttribute('gap'), GRID_GAP_VALUES, DEFAULT_GRID_GAP);
    const flow = parseEnumString(this.getAttribute('flow'), GRID_FLOW_VALUES, DEFAULT_GRID_FLOW);
    const wrapper = document.createElement('div');
    wrapper.className = composeGridClasses(cols, gap, flow);
    wrapper.appendChild(document.createElement('slot'));
    return wrapper;
  }
}

if (typeof customElements !== 'undefined' && !customElements.get('rafters-grid')) {
  customElements.define('rafters-grid', RaftersGrid);
}
