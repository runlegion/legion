import type {
  BentoPattern,
  ColumnsValue,
  GridConfig,
  GridState,
  ResponsiveColumns,
} from '@/components/ui/grid.behavior';

export interface GridClassSet {
  root: string;
}

export const gridGapClasses: Record<string, string> = {
  '0': 'gap-0',
  '1': 'gap-1',
  '2': 'gap-2',
  '3': 'gap-3',
  '4': 'gap-4',
  '5': 'gap-5',
  '6': 'gap-6',
  '8': 'gap-8',
  '10': 'gap-10',
  '12': 'gap-12',
};

export const gridPaddingClasses: Record<string, string> = {
  '0': 'p-0',
  '1': 'p-1',
  '2': 'p-2',
  '3': 'p-3',
  '4': 'p-4',
  '5': 'p-5',
  '6': 'p-6',
  '8': 'p-8',
  '10': 'p-10',
  '12': 'p-12',
};

/** Auto-scaling spacing via container queries -- Grid's default when
 *  neither gap nor padding is explicit. */
export const gridAutoSpacingClasses =
  'gap-3 p-3 @md:gap-4 @md:p-4 @lg:gap-6 @lg:p-6 @xl:gap-8 @xl:p-8';

/** Column classes. `auto` responds to the CONTAINER (one axis -- the
 *  oracle also stacked viewport defaults on top of these, a live
 *  double-axis defect, not ported). */
export const gridColumnClasses: Record<string | number, string> = {
  1: 'grid-cols-1',
  2: 'grid-cols-2',
  3: 'grid-cols-3',
  4: 'grid-cols-4',
  5: 'grid-cols-5',
  6: 'grid-cols-6',
  7: 'grid-cols-7',
  8: 'grid-cols-8',
  9: 'grid-cols-9',
  10: 'grid-cols-10',
  11: 'grid-cols-11',
  12: 'grid-cols-12',
  auto: 'grid-cols-1 @sm:grid-cols-2 @lg:grid-cols-3 @xl:grid-cols-4',
};

/**
 * Stock layouts. Placement keys off the PROJECTED data-priority attribute
 * (gridItemAttrs) -- the item declares what it is, the layout places it,
 * source order is irrelevant. The oracle's first-child selectors inferred
 * intent from position: defect-do-not-port.
 */
export const gridGoldenClasses = 'grid-cols-3 [&>[data-priority=primary]]:col-span-2';

export const gridBentoPatterns: Record<BentoPattern, string> = {
  editorial:
    'grid-cols-3 grid-rows-2 ' +
    '[&>[data-priority=primary]]:col-span-2 [&>[data-priority=primary]]:row-span-2',
  dashboard:
    'grid-cols-4 grid-rows-2 ' +
    '[&>[data-priority=primary]]:col-span-2 [&>[data-priority=primary]]:row-span-2',
  feature: 'grid-cols-2 [&>[data-priority=primary]]:row-span-2',
  portfolio:
    'grid-cols-3 grid-rows-3 ' +
    '[&>[data-priority=primary]]:col-span-2 [&>[data-priority=primary]]:row-span-2',
};

export const gridColSpanClasses: Record<number, string> = {
  1: 'col-span-1',
  2: 'col-span-2',
  3: 'col-span-3',
  4: 'col-span-4',
  5: 'col-span-5',
  6: 'col-span-6',
  7: 'col-span-7',
  8: 'col-span-8',
  9: 'col-span-9',
  10: 'col-span-10',
  11: 'col-span-11',
  12: 'col-span-12',
};

export const gridRowSpanClasses: Record<number, string> = {
  1: 'row-span-1',
  2: 'row-span-2',
  3: 'row-span-3',
};

export type ColumnsBreakpoint = 'sm' | 'md' | 'lg' | 'xl' | '2xl';

/** Per-breakpoint columns maps -- literal strings for the JIT scanner.
 *  VIEWPORT axis by documented intent (page-level layouts respond to the
 *  viewport); the container axis pending ratification. */
const gridResponsiveColumnClasses: Record<ColumnsBreakpoint, Record<string | number, string>> = {
  sm: {
    1: 'sm:grid-cols-1',
    2: 'sm:grid-cols-2',
    3: 'sm:grid-cols-3',
    4: 'sm:grid-cols-4',
    5: 'sm:grid-cols-5',
    6: 'sm:grid-cols-6',
    7: 'sm:grid-cols-7',
    8: 'sm:grid-cols-8',
    9: 'sm:grid-cols-9',
    10: 'sm:grid-cols-10',
    11: 'sm:grid-cols-11',
    12: 'sm:grid-cols-12',
  },
  md: {
    1: 'md:grid-cols-1',
    2: 'md:grid-cols-2',
    3: 'md:grid-cols-3',
    4: 'md:grid-cols-4',
    5: 'md:grid-cols-5',
    6: 'md:grid-cols-6',
    7: 'md:grid-cols-7',
    8: 'md:grid-cols-8',
    9: 'md:grid-cols-9',
    10: 'md:grid-cols-10',
    11: 'md:grid-cols-11',
    12: 'md:grid-cols-12',
  },
  lg: {
    1: 'lg:grid-cols-1',
    2: 'lg:grid-cols-2',
    3: 'lg:grid-cols-3',
    4: 'lg:grid-cols-4',
    5: 'lg:grid-cols-5',
    6: 'lg:grid-cols-6',
    7: 'lg:grid-cols-7',
    8: 'lg:grid-cols-8',
    9: 'lg:grid-cols-9',
    10: 'lg:grid-cols-10',
    11: 'lg:grid-cols-11',
    12: 'lg:grid-cols-12',
  },
  xl: {
    1: 'xl:grid-cols-1',
    2: 'xl:grid-cols-2',
    3: 'xl:grid-cols-3',
    4: 'xl:grid-cols-4',
    5: 'xl:grid-cols-5',
    6: 'xl:grid-cols-6',
    7: 'xl:grid-cols-7',
    8: 'xl:grid-cols-8',
    9: 'xl:grid-cols-9',
    10: 'xl:grid-cols-10',
    11: 'xl:grid-cols-11',
    12: 'xl:grid-cols-12',
  },
  '2xl': {
    1: '2xl:grid-cols-1',
    2: '2xl:grid-cols-2',
    3: '2xl:grid-cols-3',
    4: '2xl:grid-cols-4',
    5: '2xl:grid-cols-5',
    6: '2xl:grid-cols-6',
    7: '2xl:grid-cols-7',
    8: '2xl:grid-cols-8',
    9: '2xl:grid-cols-9',
    10: '2xl:grid-cols-10',
    11: '2xl:grid-cols-11',
    12: '2xl:grid-cols-12',
  },
};

const breakpointOrder: ColumnsBreakpoint[] = ['sm', 'md', 'lg', 'xl', '2xl'];

export function resolveColumnsClasses(columns: ResponsiveColumns | undefined): string {
  if (columns === undefined) return gridColumnClasses['auto'] ?? '';

  if (typeof columns === 'number' || columns === 'auto') {
    return gridColumnClasses[columns] ?? '';
  }

  const parts: string[] = [];
  if (columns.base !== undefined) {
    const base = gridColumnClasses[columns.base];
    if (base) parts.push(base);
  }
  for (const breakpoint of breakpointOrder) {
    const value: ColumnsValue | undefined = columns[breakpoint];
    if (value !== undefined) {
      const cls = gridResponsiveColumnClasses[breakpoint][value];
      if (cls) parts.push(cls);
    }
  }
  return parts.join(' ');
}

export function gridClasses(config: GridConfig, _state: GridState): GridClassSet {
  const preset = config.preset ?? 'linear';
  const autoSpacing = config.gap === undefined && config.padding === undefined;

  const parts: string[] = ['grid'];
  if (autoSpacing) {
    parts.push(gridAutoSpacingClasses);
  } else {
    if (config.gap !== undefined) parts.push(gridGapClasses[config.gap] ?? '');
    if (config.padding !== undefined) parts.push(gridPaddingClasses[config.padding] ?? '');
  }

  if (preset === 'linear') parts.push(resolveColumnsClasses(config.columns));
  if (preset === 'golden') parts.push(gridGoldenClasses);
  if (preset === 'bento' && config.pattern) parts.push(gridBentoPatterns[config.pattern]);

  return { root: parts.filter(Boolean).join(' ') };
}
