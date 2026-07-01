/**
 * <rafters-container> -- Web Component layout primitive.
 *
 * Mirrors the semantics of container.tsx / container.astro (size, padding,
 * gap, background, article typography, editable). The inner semantic element
 * carries the SAME utility class strings the React and Astro targets use --
 * imported from container.classes.ts -- rather than a parallel hand-written
 * CSS map. Visual presentation comes from the shared compiled utility
 * stylesheet adopted by RaftersElement (see setUtilityCSS) plus the token
 * custom properties inherited from the host :root.
 *
 * Container queries are carried on the inner element via the query-container
 * utility (containerQueryClasses), exactly as in the React/Astro targets, so
 * the responsive auto edge padding and any descendant queries resolve against
 * the same query container the other targets use. The inner element also
 * carries the full-bleed sizing utility, so the host needs no sizing CSS.
 *
 * The only shadow-scoped CSS this component owns is the structural :host
 * display shim: custom elements default to inline display, but the inner
 * semantic element is the outer block-level box the React/Astro element is.
 *
 * Attributes:
 *  - as: 'div' | 'main' | 'header' | 'footer' | 'section' | 'article' | 'aside' (default 'div')
 *  - size: 'sm'..'7xl' | 'full'
 *  - padding: '0' | '1' | ... | '24'
 *  - gap: same as padding scale, OR bare/empty -> derive from size
 *  - col-span: 1..12 -- grid column span when placed directly in a grid
 *  - row-span: 1..3 -- grid row span when placed directly in a grid
 *  - position: 'sticky' | 'fixed' | 'relative' | 'absolute' | 'static'
 *  - depth: 'base' | 'dropdown' | 'sticky' | 'navigation' | 'fixed' | 'modal' | 'popover' | 'tooltip' | 'overlay' | 'below' | 'max'
 *  - background: 'none' | 'muted' | 'accent' | 'card' | 'primary'
 *  - editable: boolean (presence-based)
 */
import { RaftersElement } from '@/lib/primitives/rafters-element';
import {
  type ContainerBackground,
  type ContainerDepth,
  type ContainerPosition,
  containerArticleTypography,
  containerAutoEdgePadding,
  containerBackgroundClasses,
  containerCenterClasses,
  containerDepthClasses,
  containerEditableClasses,
  containerGapClasses,
  containerPaddingClasses,
  containerPositionClasses,
  containerQueryClasses,
  containerSizeClasses,
  containerSizeGapScale,
} from '@/components/ui/container.classes';
import { gridColSpanClasses, gridRowSpanClasses } from '@/components/ui/grid.classes';

export type ContainerAs = 'div' | 'main' | 'header' | 'footer' | 'section' | 'article' | 'aside';

export type ContainerColSpan = 1 | 2 | 3 | 4 | 5 | 6 | 7 | 8 | 9 | 10 | 11 | 12;
export type ContainerRowSpan = 1 | 2 | 3;

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

export type ContainerSpacing =
  | '0'
  | '1'
  | '2'
  | '3'
  | '4'
  | '5'
  | '6'
  | '8'
  | '10'
  | '12'
  | '16'
  | '20'
  | '24';

const ALLOWED_AS: ReadonlyArray<ContainerAs> = [
  'div',
  'main',
  'header',
  'footer',
  'section',
  'article',
  'aside',
];

const SIZES: ReadonlyArray<ContainerSize> = [
  'sm',
  'md',
  'lg',
  'xl',
  '2xl',
  '3xl',
  '4xl',
  '5xl',
  '6xl',
  '7xl',
  'full',
];

const SPACING: ReadonlyArray<ContainerSpacing> = [
  '0',
  '1',
  '2',
  '3',
  '4',
  '5',
  '6',
  '8',
  '10',
  '12',
  '16',
  '20',
  '24',
];

const POSITIONS: ReadonlyArray<ContainerPosition> = [
  'sticky',
  'fixed',
  'relative',
  'absolute',
  'static',
];

const DEPTHS: ReadonlyArray<ContainerDepth> = [
  'base',
  'dropdown',
  'sticky',
  'navigation',
  'fixed',
  'modal',
  'popover',
  'tooltip',
  'overlay',
  'below',
  'max',
];

const BACKGROUNDS: ReadonlyArray<ContainerBackground> = [
  'none',
  'muted',
  'accent',
  'card',
  'primary',
];

const COL_SPANS: ReadonlyArray<ContainerColSpan> = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12];

const ROW_SPANS: ReadonlyArray<ContainerRowSpan> = [1, 2, 3];

function isContainerSize(value: string | null): value is ContainerSize {
  return value !== null && (SIZES as ReadonlyArray<string>).includes(value);
}

function isContainerSpacing(value: string | null): value is ContainerSpacing {
  return value !== null && (SPACING as ReadonlyArray<string>).includes(value);
}

function isContainerBackground(value: string | null): value is ContainerBackground {
  return value !== null && (BACKGROUNDS as ReadonlyArray<string>).includes(value);
}

function parseAs(value: string | null): ContainerAs {
  if (value && (ALLOWED_AS as ReadonlyArray<string>).includes(value)) {
    return value as ContainerAs;
  }
  return 'div';
}

function parseSize(value: string | null): ContainerSize | undefined {
  return isContainerSize(value) ? value : undefined;
}

function parsePadding(value: string | null): ContainerSpacing | undefined {
  return isContainerSpacing(value) ? value : undefined;
}

function parseBackground(value: string | null): ContainerBackground | undefined {
  if (!isContainerBackground(value)) return undefined;
  return value === 'none' ? undefined : value;
}

function parseGap(value: string | null): ContainerSpacing | true | undefined {
  if (value === null) return undefined;
  if (value === '') return true;
  return isContainerSpacing(value) ? value : undefined;
}

function parsePosition(value: string | null): ContainerPosition | undefined {
  if (value !== null && (POSITIONS as ReadonlyArray<string>).includes(value)) {
    return value as ContainerPosition;
  }
  return undefined;
}

function parseDepth(value: string | null): ContainerDepth | undefined {
  if (value !== null && (DEPTHS as ReadonlyArray<string>).includes(value)) {
    return value as ContainerDepth;
  }
  return undefined;
}

function parseSpan<T extends number>(
  value: string | null,
  allowed: ReadonlyArray<T>,
): T | undefined {
  if (value === null) return undefined;
  const parsed = Number.parseInt(value, 10);
  return (allowed as ReadonlyArray<number>).includes(parsed) ? (parsed as T) : undefined;
}

export interface ContainerClassOptions {
  size?: ContainerSize | undefined;
  padding?: ContainerSpacing | undefined;
  gap?: ContainerSpacing | true | undefined;
  position?: ContainerPosition | undefined;
  depth?: ContainerDepth | undefined;
  background?: ContainerBackground | undefined;
  colSpan?: ContainerColSpan | undefined;
  rowSpan?: ContainerRowSpan | undefined;
  article?: boolean | undefined;
  editable?: boolean | undefined;
}

/**
 * Resolve the bare-gap default for the given size, walking the spacing scale.
 * Falls back to '6' when no size set or size is 'full'.
 */
function resolveDerivedGap(size: ContainerSize | undefined): ContainerSpacing {
  if (size && size !== 'full') {
    return containerSizeGapScale[size] as ContainerSpacing;
  }
  return '6';
}

export function composeContainerClasses(options: ContainerClassOptions): string {
  const { size, padding, gap, position, depth, background, colSpan, rowSpan, article, editable } =
    options;
  const parts: string[] = [];
  parts.push(containerQueryClasses);
  if (size) parts.push(containerSizeClasses[size] as string);
  if (size && size !== 'full') parts.push(containerCenterClasses);
  if (padding) {
    parts.push(containerPaddingClasses[padding] as string);
  } else if (size && size !== 'full') {
    parts.push(containerAutoEdgePadding);
  }
  let resolvedGap: ContainerSpacing | null = null;
  if (gap === true) {
    resolvedGap = resolveDerivedGap(size);
  } else if (typeof gap === 'string' && SPACING.includes(gap)) {
    resolvedGap = gap;
  }
  if (resolvedGap !== null) {
    parts.push(containerGapClasses[resolvedGap] as string);
  }
  if (colSpan) parts.push(gridColSpanClasses[colSpan] as string);
  if (rowSpan) parts.push(gridRowSpanClasses[rowSpan] as string);
  if (position) parts.push(containerPositionClasses[position]);
  if (depth) parts.push(containerDepthClasses[depth]);
  if (background && background !== 'none') {
    parts.push(containerBackgroundClasses[background as ContainerBackground]);
  }
  if (article) parts.push(containerArticleTypography);
  if (editable) parts.push(containerEditableClasses);
  return parts.filter(Boolean).join(' ');
}
const OBSERVED_ATTRIBUTES: ReadonlyArray<string> = [
  'as',
  'size',
  'padding',
  'gap',
  'col-span',
  'row-span',
  'position',
  'depth',
  'background',
  'editable',
] as const;

export class RaftersContainer extends RaftersElement {
  static observedAttributes: ReadonlyArray<string> = OBSERVED_ATTRIBUTES;

  /**
   * The only component-owned CSS: the structural host-display shim. The inner
   * semantic element is the outer block-level box; the host presents as a
   * block so the inner element fills the available inline space.
   */
  static override styles = ':host { display: block; }';

  /**
   * Resolve the semantic element tag from the `as` attribute, falling back to
   * `div` for anything outside the allow-list.
   */
  getAs(): ContainerAs {
    return parseAs(this.getAttribute('as'));
  }

  /**
   * Render the inner semantic element with a single default <slot>.
   * DOM APIs only -- never innerHTML. The inner element carries the shared
   * utility classes; presentation resolves from the adopted utility sheet.
   */
  override render(): Node {
    const inner = document.createElement(this.getAs());
    inner.className = composeContainerClasses({
      size: parseSize(this.getAttribute('size')),
      padding: parsePadding(this.getAttribute('padding')),
      gap: parseGap(this.getAttribute('gap')),
      position: parsePosition(this.getAttribute('position')),
      depth: parseDepth(this.getAttribute('depth')),
      background: parseBackground(this.getAttribute('background')),
      colSpan: parseSpan(this.getAttribute('col-span'), COL_SPANS),
      rowSpan: parseSpan(this.getAttribute('row-span'), ROW_SPANS),
      article: this.getAs() === 'article',
      editable: this.hasAttribute('editable'),
    });
    inner.appendChild(document.createElement('slot'));
    return inner;
  }
}
if (!customElements.get('rafters-container')) {
  customElements.define('rafters-container', RaftersContainer);
}
