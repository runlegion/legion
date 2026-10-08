import type {
  ChartConfig,
  ChartContainerConfig,
  ChartContainerState,
  ChartToken,
} from '@/components/ui/chart.behavior';

export interface ChartContainerClassSet {
  root: string;
  plot: string;
}

/**
 * Literal fill classes, one per token (Spec 01: classes.ts selects among
 * literals, it never constructs one -- Tailwind's scanner must see every
 * emitted class verbatim in source). Never `fill-${token}` at runtime.
 */
const FILL_CLASS_BY_TOKEN: Record<ChartToken, string> = {
  'chart-1': 'fill-chart-1',
  'chart-2': 'fill-chart-2',
  'chart-3': 'fill-chart-3',
  'chart-4': 'fill-chart-4',
  'chart-5': 'fill-chart-5',
};

/** Same literal discipline over the stroke channel, for line-drawing series
 *  (Line #2226, Area #2227). */
const STROKE_CLASS_BY_TOKEN: Record<ChartToken, string> = {
  'chart-1': 'stroke-chart-1',
  'chart-2': 'stroke-chart-2',
  'chart-3': 'stroke-chart-3',
  'chart-4': 'stroke-chart-4',
  'chart-5': 'stroke-chart-5',
};

const CHART_TOKENS: readonly ChartToken[] = ['chart-1', 'chart-2', 'chart-3', 'chart-4', 'chart-5'];

/** A token-less series falls back to `chart-N` by series index, wrapping
 *  past 5 (documented in the issue's behavior section). `index` wins when
 *  supplied; otherwise it is the key's position in `config`, clamped to 0
 *  when the key is not a member (defensive -- never a negative modulo). */
function fallbackTokenIndex(config: ChartConfig, key: string, index: number | undefined): number {
  const position = index ?? Object.keys(config).indexOf(key);
  const normalized = position >= 0 ? position : 0;
  return normalized % CHART_TOKENS.length;
}

/** Shared resolution shape for both color channels: an explicit `token` maps
 *  straight to the channel's literal class; a token-less series falls back
 *  to `chart-N` by series index, never to a hex or `var()`. `resolveSeriesClass`
 *  and `resolveSeriesStrokeClass` are one-line callers over their own literal
 *  map -- the map itself must still be a fully literal `Record` (Spec 01), so
 *  the sharing stops at the lookup, never at the literals. */
function resolveFromMap(
  map: Record<ChartToken, string>,
  config: ChartConfig,
  key: string,
  index: number | undefined,
): string {
  const token = config[key]?.token;
  if (token) return map[token];
  const tokenIndex = fallbackTokenIndex(config, key, index);
  return map[CHART_TOKENS[tokenIndex] as ChartToken];
}

/**
 * Resolve a series' fill class: an explicit `token` maps to its literal
 * `fill-chart-N`; a token-less series falls back to `chart-N` by series
 * index, never to a hex or `var()`.
 */
export function resolveSeriesClass(config: ChartConfig, key: string, index?: number): string {
  return resolveFromMap(FILL_CLASS_BY_TOKEN, config, key, index);
}

/** Same resolution over the stroke channel. */
export function resolveSeriesStrokeClass(config: ChartConfig, key: string, index?: number): string {
  return resolveFromMap(STROKE_CLASS_BY_TOKEN, config, key, index);
}

/**
 * ChartContainer classes: no color, spacing, or motion value is authored
 * here -- Container/Grid own layout (Boundary 00 sec 5 corollary), and the
 * one design-owned datum the issue names (a ratio/min-height token) has no
 * registry entry yet (verified against design-tokens/src/generators/
 * defaults.ts: only the five `chart-N` COLOR tokens exist). Recorded as a
 * spec gap in the PR rather than invented here. The only classes emitted are
 * structural fill-parent sizing, needed for the plot region to have a
 * measurable box for `observeResize` to report against.
 */
export function chartContainerClasses(
  _config: ChartContainerConfig,
  _state: ChartContainerState,
): ChartContainerClassSet {
  return { root: 'w-full', plot: 'w-full h-full' };
}
