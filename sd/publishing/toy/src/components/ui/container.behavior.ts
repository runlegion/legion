import type { BehaviorSpec } from '@/lib/contract';
import type { ResponsiveColumns, SpacingValue } from '@/components/ui/grid.behavior';

/**
 * Container: the div-soup killer. A static score -- no state, no actions,
 * no keymap, no effects. Its contract is structural: the semantic element
 * carries the landmark (main = primary landmark, header/footer = banners,
 * aside = complementary, article = readable content with typography,
 * section + aria-label = named region -- all NATIVE semantics from the
 * element choice, which is why the aria projection is empty), spacing
 * happens inside (padding/gap, never margins), and the element is a
 * container-query provider by default.
 *
 * Layout modes are exclusive and mechanical (ruled 2026-07-03):
 * - `columns` present -> grid mode (same 1-12 vocabulary as Grid linear;
 *   `gap` means grid gap). One tag, container + grid.
 * - no `columns`, `gap` present -> vertical stack (flex-col; gap=true
 *   derives from size by walking the spacing scale).
 * - neither -> plain block.
 */

export type ContainerElement =
  | 'div'
  | 'main'
  | 'header'
  | 'footer'
  | 'section'
  | 'article'
  | 'aside';

export type ContainerSize =
  | 'sm'
  | 'md'
  | 'lg'
  | 'xl'
  | '2xl'
  | '3xl'
  | '4xl'
  | '5xl'
  | '6xl'
  | '7xl'
  | 'full';

export type ContainerPadding = SpacingValue | '16' | '20' | '24';

export type ContainerPosition = 'sticky' | 'fixed' | 'relative' | 'absolute' | 'static';

export type ContainerDepth =
  | 'base'
  | 'dropdown'
  | 'sticky'
  | 'navigation'
  | 'fixed'
  | 'modal'
  | 'popover'
  | 'tooltip'
  | 'overlay'
  | 'below'
  | 'max';

export interface ContainerConfig {
  as?: ContainerElement | undefined;
  size?: ContainerSize | undefined;
  padding?: ContainerPadding | undefined;
  /** Vertical stack gap; true derives from size. Grid gap when columns set. */
  gap?: boolean | ContainerPadding | undefined;
  /** Grid mode: linear columns, same vocabulary as Grid (1-12, responsive
   *  object, auto). Presets stay Grid's. */
  columns?: ResponsiveColumns | undefined;
  /** Container-query provider. Default true. */
  query?: boolean | undefined;
  queryName?: string | undefined;
  /** Self-placement when this Container is a grid child. */
  colSpan?: 1 | 2 | 3 | 4 | 5 | 6 | 7 | 8 | 9 | 10 | 11 | 12 | undefined;
  rowSpan?: 1 | 2 | 3 | undefined;
  position?: ContainerPosition | undefined;
  depth?: ContainerDepth | undefined;
  /** Fill signature over the color vocabulary (#1637). */
  fill?: string | undefined;
}

export type ContainerState = Record<never, never>;
export type ContainerActions = Record<never, never>;
export type ContainerPart = 'root';

export const container: BehaviorSpec<
  ContainerConfig,
  ContainerState,
  ContainerActions,
  ContainerPart
> = {
  name: 'container',
  parts: { root: {} },
  initialState: () => ({}),
  actions: {},
  canDispatch: () => true,
  // Landmark semantics are native to the element the config chooses; the
  // score projects nothing and the harness asserts the ELEMENT contract.
  aria: () => ({ root: {} }),
  keymap: () => null,
};
