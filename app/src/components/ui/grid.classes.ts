/**
 * Shared class definitions for Grid component
 * Used by both grid.tsx (React) and grid.astro (Astro)
 */

export type GridPreset = 'linear' | 'golden' | 'bento';
export type BentoPattern = 'editorial' | 'dashboard' | 'feature' | 'portfolio';

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

/**
 * Auto-scaling gap and padding via container queries.
 * Grid's default behavior: spacing scales with the container size.
 * Explicit gap/padding props override this.
 */
export const gridAutoSpacingClasses =
  'gap-3 p-3 @md:gap-4 @md:p-4 @lg:gap-6 @lg:p-6 @xl:gap-8 @xl:p-8';

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

export const gridBentoPatterns: Record<BentoPattern, string> = {
  editorial: 'grid-cols-3 grid-rows-2 [&>*:first-child]:col-span-2 [&>*:first-child]:row-span-2',
  dashboard: 'grid-cols-4 grid-rows-2 [&>*:first-child]:col-span-2 [&>*:first-child]:row-span-2',
  feature: 'grid-cols-2 [&>*:first-child]:row-span-2',
  portfolio: 'grid-cols-3 grid-rows-3 [&>*:first-child]:col-span-2 [&>*:first-child]:row-span-2',
};

export const gridGoldenClasses = 'grid-cols-3 [&>*:first-child]:col-span-2';

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

// ============================================================================
// Responsive columns
// ============================================================================

/**
 * A single columns value: 1-12 or 'auto' (the existing auto-fit behavior).
 */
export type ColumnsValue = 1 | 2 | 3 | 4 | 5 | 6 | 7 | 8 | 9 | 10 | 11 | 12 | 'auto';

/**
 * Tailwind viewport breakpoint prefix used for responsive columns.
 * Page-level marketing layouts respond to viewport, not container.
 */
export type ColumnsBreakpoint = 'sm' | 'md' | 'lg' | 'xl' | '2xl';

/**
 * Responsive columns config: a base value plus optional per-breakpoint overrides.
 * Example: `{ base: 2, md: 4 }` becomes `grid-cols-2 md:grid-cols-4`.
 */
export interface ResponsiveColumnsObject {
  base?: ColumnsValue;
  sm?: ColumnsValue;
  md?: ColumnsValue;
  lg?: ColumnsValue;
  xl?: ColumnsValue;
  '2xl'?: ColumnsValue;
}

/**
 * Either a single columns value or a responsive object.
 */
export type ResponsiveColumns = ColumnsValue | ResponsiveColumnsObject;

/**
 * Per-breakpoint columns class maps. Tailwind needs these as literal strings
 * for the JIT scanner.
 */
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

/**
 * Resolve a `columns` prop (single value or responsive object) to a class string.
 *
 * - Single value: returns the matching grid-cols class.
 * - Object: returns base + per-breakpoint classes joined by spaces.
 *
 * Returns empty string when no resolvable classes are produced.
 */
export function resolveColumnsClasses(columns: ResponsiveColumns | undefined): string {
  if (columns === undefined) return '';

  if (typeof columns === 'number' || columns === 'auto') {
    return gridColumnClasses[columns] ?? '';
  }

  const parts: string[] = [];
  if (columns.base !== undefined) {
    const base = gridColumnClasses[columns.base];
    if (base) parts.push(base);
  }
  for (const bp of breakpointOrder) {
    const value = columns[bp];
    if (value !== undefined) {
      const cls = gridResponsiveColumnClasses[bp][value];
      if (cls) parts.push(cls);
    }
  }
  return parts.join(' ');
}

/**
 * True when the columns prop carries no useful 'auto' fallback. Used by Grid to
 * decide whether to apply its default responsive columns when the consumer
 * passes 'auto' or omits the prop entirely.
 */
export function columnsResolvesToAuto(columns: ResponsiveColumns | undefined): boolean {
  if (columns === undefined) return true;
  if (columns === 'auto') return true;
  return false;
}
