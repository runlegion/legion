import { z } from 'zod';
import type { AriaAttrs, BehaviorSpec, PartIds } from '@/lib/contract';
import { createBehavior } from '@/lib/contract';
import { updateAriaAttribute } from '@/lib/primitives/aria-manager';
import { observeResize } from '@/lib/primitives/graph';

/**
 * ChartContainer: the shadcn-compatible container and token-mapped series
 * config every chart component composes (#2224). A static score -- no
 * keymap, no consumer-dispatched actions -- but the measured plot size IS
 * state: `resize` is a programmatic action the DOM-native client and the
 * React performance both dispatch from the composed `observeResize`
 * primitive (Boundary 00 sec 7: impure work is composed, never hand-rolled
 * per performance).
 *
 * Series color is a token NAME (chart-1..chart-5), never a hex or `var()` --
 * the only categorical color surface (Boundary 00 sec 6). Resolution into
 * literal `fill-chart-N` classes lives in chart.classes.ts, never here (Spec
 * 01 rule 1: behavior.ts imports primitives and sibling behavior types only,
 * never classes.ts).
 *
 * The category key lives on `<XAxis dataKey>` (x-axis.behavior.ts), never on
 * this config -- veneer's compositional-children amendment (bullpen
 * 01a058ec): axes/grid/legend are children, never props, so a chart is
 * axis-less by omission, not by a minimal flag.
 */

// -- Token vocabulary (Boundary 00 section 6) --------------------------------

export type ChartToken = 'chart-1' | 'chart-2' | 'chart-3' | 'chart-4' | 'chart-5';

// -- Config types (shadcn-compatible; token replaces shadcn's hex/var()) ----

export interface ChartSeriesConfig {
  /** Human-readable series label (legend/tooltip, #2228). */
  label?: string | undefined;
  /** Optional icon component; the concrete type is framework-owned. */
  icon?: unknown;
  /** Series color token; designer-mapped, resolved in chart.classes.ts. */
  token?: ChartToken | undefined;
}

/** Keyed by series/data key, exactly like shadcn's ChartConfig. */
export type ChartConfig = Record<string, ChartSeriesConfig>;

/** ChartContainer config (consumer choices). */
export interface ChartContainerConfig {
  config: ChartConfig;
}

// -- Zod validation (external data boundary, CLAUDE.md) ----------------------

const chartTokenSchema = z.enum(['chart-1', 'chart-2', 'chart-3', 'chart-4', 'chart-5']);

const chartSeriesConfigSchema = z.object({
  label: z.string().optional(),
  icon: z.unknown().optional(),
  token: chartTokenSchema.optional(),
});

const chartConfigSchema = z.record(z.string(), chartSeriesConfigSchema);

/** Walk a raw input by a Zod issue path to recover the value that was
 *  actually rejected -- so the thrown message can name it verbatim rather
 *  than repeating Zod's generic "expected one of ..." text. */
function valueAtPath(input: unknown, path: ReadonlyArray<PropertyKey>): unknown {
  return path.reduce<unknown>((value, segment) => {
    if (value !== null && typeof value === 'object') {
      return (value as Record<PropertyKey, unknown>)[segment];
    }
    return undefined;
  }, input);
}

/**
 * Parse and validate a ChartConfig at the external-data boundary. An
 * out-of-range token (e.g. "chart-6") or malformed series entry fails LOUD,
 * naming both the offending key path and the rejected value, so a typo
 * surfaces here rather than degrading silently at render. An empty object is
 * a valid, empty config -- renders an empty plot, never throws.
 */
export function parseChartConfig(input: unknown): ChartConfig {
  const result = chartConfigSchema.safeParse(input);
  if (result.success) return result.data;

  const issue = result.error.issues[0];
  const path = issue?.path ?? [];
  const keyPath = path.length > 0 ? path.map(String).join('.') : '(root)';
  const received = valueAtPath(input, path);
  const detail =
    received !== undefined
      ? `received ${JSON.stringify(received)}`
      : (issue?.message ?? 'validation failed');
  throw new Error(`Invalid ChartConfig at key "${keyPath}": ${detail}`);
}

/**
 * Resolve a series' display label from `ChartConfig`, falling back when the
 * key is absent or carries no `label`. Additive export (#2228): both
 * `chart-tooltip.behavior.ts` and `chart-legend.behavior.ts` read a series
 * label the same way (a row's own key, or a `nameKey`/`labelKey` override),
 * so this lives once here rather than as two identical private helpers.
 */
export function resolveSeriesLabel(config: ChartConfig, key: string, fallback: string): string {
  return config[key]?.label ?? fallback;
}

// -- Behavior spec (measured size is state; no keymap, no dispatched actions) -

export interface ChartSize {
  width: number;
  height: number;
}

export type ChartContainerState = ChartSize;
export type ChartContainerActions = { resize: ChartSize };
export type ChartContainerPart = 'root' | 'plot';

export const chartContainer: BehaviorSpec<
  ChartContainerConfig,
  ChartContainerState,
  ChartContainerActions,
  ChartContainerPart
> = {
  name: 'chart-container',
  parts: {
    root: {},
    plot: {},
  },
  initialState: () => ({ width: 0, height: 0 }),
  actions: {
    // Effective-value-diff convention (Spec 01): a same-value resize is a
    // no-op so `memory.subscribe` does not fan out redundant renders.
    resize: (state, payload) =>
      state.width === payload.width && state.height === payload.height ? state : payload,
  },
  canDispatch: () => true,
  // Neither part carries semantics of its own -- the plot is a measured box,
  // not a landmark -- so the projection is empty and the harness asserts the
  // empty contract, same shape as container/aspect-ratio.
  aria: () => ({ root: {}, plot: {} }),
  keymap: () => null,
};

// -- DOM-native client (WC + Astro share this) --------------------------------

/**
 * Apply a projected `AriaAttrs` object to an element via the `aria-manager`
 * primitive (`updateAriaAttribute`) -- composed, not a hand-rolled
 * `setAttribute` loop. Additive export (#2228): `bindChart` below still
 * inlines its own copy (pre-existing, untouched here); `chart-tooltip.behavior.ts`
 * and `chart-legend.behavior.ts` both import THIS one rather than each
 * defining their own, which is what made it worth naming.
 */
export function applyAriaProjection(el: HTMLElement, attrs: AriaAttrs): void {
  for (const [name, value] of Object.entries(attrs)) {
    updateAriaAttribute(el, name as never, value as never, { validate: false });
  }
}

/**
 * The DOM-native binding of the chart-container score. Reads `ChartConfig`
 * from the root's `data-config` JSON attribute (the WC/Astro transport
 * pinned by this issue), composes `observeResize` (graph.ts, #2223) directly
 * on the plot part -- one primitive, so no colocated composition function is
 * needed (05-authoring: a direct call when it is one primitive) -- and
 * republishes the measured size onto the root's dataset for child binds to
 * read (`data-chart-width` / `data-chart-height`).
 */
export function bindChart(root: HTMLElement): () => void {
  const configAttr = root.getAttribute('data-config');
  const containerConfig: ChartContainerConfig = {
    config: parseChartConfig(configAttr ? JSON.parse(configAttr) : {}),
  };

  const getPart = (part: ChartContainerPart): HTMLElement | null =>
    part === 'root' ? root : root.querySelector<HTMLElement>(`[data-part="${part}"]`);

  const { memory, dispatch } = createBehavior(chartContainer, containerConfig);

  const ids: PartIds<ChartContainerPart> = {
    root: root.id || '',
    plot: getPart('plot')?.id ?? '',
  };

  const applyProjection = (el: HTMLElement, attrs: AriaAttrs) => {
    for (const [name, value] of Object.entries(attrs)) {
      updateAriaAttribute(el, name as never, value as never, { validate: false });
    }
  };

  const render = () => {
    const state = memory.get();
    const projection = chartContainer.aria(state, containerConfig, ids);
    for (const part of Object.keys(projection) as ChartContainerPart[]) {
      const attrs = projection[part];
      const el = getPart(part);
      if (el && attrs) applyProjection(el, attrs);
    }
    // Expose measured size on the root for child binds (#2225-2227 read this
    // rather than re-observing the plot themselves).
    root.dataset['chartWidth'] = String(state.width);
    root.dataset['chartHeight'] = String(state.height);
  };
  const unsubscribe = memory.subscribe(render); // fires immediately: first paint

  const plotEl = getPart('plot') ?? root;
  const stopObserving = observeResize(plotEl, (size) => {
    dispatch('resize', containerConfig, size);
  });

  return () => {
    unsubscribe();
    stopObserving();
  };
}
