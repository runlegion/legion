/**
 * Chart container and configuration surface for the rafters chart family
 *
 * @cognitive-load 2/10 - Low decision surface: config maps series to tokens,
 * the container provides a measured plot region. The consumer decides which
 * chart type and which compositional children (axes, grid, legend) to include.
 * @attention-economics Invisible scaffolding: the container is structural,
 * never visual. Attention stays on the chart content it frames.
 * @trust-building Consistent token-mapped colors across every chart type;
 * config is validated at the boundary, so an out-of-range token fails at
 * construction time, never as a silent wrong color at render.
 * @accessibility Provides the structural grouping and measured size the
 * chart family's accessible pattern builds on (figure + aria-hidden SVG +
 * sr-announcer, pinned in Bar #2225); this component's own projection is
 * empty -- neither the root nor the plot region carries semantics of its own.
 * @semantic-meaning The configuration bridge between shadcn's ChartConfig
 * shape and rafters' token vocabulary. One declared divergence: token NAMES
 * (chart-1..chart-5) replace shadcn's hex/var() color values.
 *
 * @usage-patterns
 * DO: Wrap every chart in ChartContainer with a ChartConfig
 * DO: Use token names (chart-1..chart-5) in ChartConfig, never hex or var()
 * DO: Compose XAxis/YAxis/CartesianGrid as children when a chart needs them
 * DO: Omit axis/grid children for axis-less charts (sparklines) -- omission,
 *     never a flag
 * NEVER: Pass hex colors, arbitrary values, or var() in config
 * NEVER: Add a categoryKey to config -- it belongs on <XAxis dataKey>
 *
 * @example
 * ```tsx
 * const config = {
 *   desktop: { label: 'Desktop', token: 'chart-1' },
 *   mobile: { label: 'Mobile', token: 'chart-2' },
 * } satisfies ChartConfig;
 *
 * <ChartContainer config={config}>
 *   <CartesianGrid />
 *   <XAxis dataKey="month" />
 *   <YAxis />
 *   {// Bar/Line/Area (#2225-2227) render the marks against config + size}
 * </ChartContainer>
 * ```
 */
import * as React from 'react';
import classy from '@/lib/primitives/classy';
import { observeResize } from '@/lib/primitives/graph';
import type { ChartConfig, ChartContainerConfig } from '@/components/ui/chart.behavior';
import { chartContainerClasses } from '@/components/ui/chart.classes';

export interface ChartSize {
  width: number;
  height: number;
}

const ZERO_SIZE: ChartSize = { width: 0, height: 0 };

const ChartConfigContext = React.createContext<ChartConfig>({});
const ChartSizeContext = React.createContext<ChartSize>(ZERO_SIZE);

/** The config a `ChartContainer` ancestor provided; `{}` outside one. */
export function useChartConfig(): ChartConfig {
  return React.useContext(ChartConfigContext);
}

/** The plot region's measured size, updated on every `observeResize`
 *  callback; `{ width: 0, height: 0 }` before first layout or outside a
 *  `ChartContainer`. */
export function useChartSize(): ChartSize {
  return React.useContext(ChartSizeContext);
}

export interface ChartContainerProps extends React.HTMLAttributes<HTMLDivElement> {
  config: ChartConfig;
}

export const ChartContainer = React.forwardRef<HTMLDivElement, ChartContainerProps>(
  ({ config, className, children, ...props }, ref) => {
    const containerConfig: ChartContainerConfig = { config };
    const plotRef = React.useRef<HTMLDivElement>(null);
    const [size, setSize] = React.useState<ChartSize>(ZERO_SIZE);
    // #2243 deferred low finding: this was ZERO_SIZE unconditionally, so a
    // future state-dependent class in chartContainerClasses would have been
    // silently stuck at the pre-layout value for the life of the component.
    // chartContainerClasses ignores its state parameter today (chart.classes.ts),
    // so this is behavior-neutral now and only matters once that changes.
    const classes = chartContainerClasses(containerConfig, size);

    // Composed directly (one primitive -- 05-authoring: no colocated
    // composition function needed until a second primitive joins it). The
    // WC/Astro client (bindChart) composes the same primitive on the plot
    // part; this is React's performance of that composition, not a
    // reimplementation of it.
    React.useEffect(() => {
      const el = plotRef.current;
      if (!el) return;
      return observeResize(el, (next) => {
        setSize((prev) => (prev.width === next.width && prev.height === next.height ? prev : next));
      });
    }, []);

    return (
      <ChartConfigContext.Provider value={config}>
        <ChartSizeContext.Provider value={size}>
          <div
            ref={ref}
            data-part="root"
            className={classy(classes.root, className) || undefined}
            {...props}
          >
            <div ref={plotRef} data-part="plot" className={classy(classes.plot) || undefined}>
              {children}
            </div>
          </div>
        </ChartSizeContext.Provider>
      </ChartConfigContext.Provider>
    );
  },
);

ChartContainer.displayName = 'ChartContainer';

export default ChartContainer;
