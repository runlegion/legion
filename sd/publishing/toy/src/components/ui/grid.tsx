/**
 * Intelligent layout grid with semantic presets and embedded design reasoning
 *
 * @cognitive-load 4/10 - Layout container with intelligent presets that respect Miller's Law
 * @attention-economics Preset hierarchy: linear=democratic attention, golden=hierarchical flow, bento=complex attention patterns
 * @trust-building Mathematical spacing, Miller's Law cognitive load limits, consistent preset behavior
 * @accessibility WCAG AAA compliance with optional ARIA grid role for interactive layouts
 * @semantic-meaning Layout intelligence: linear=equal-priority content, golden=natural hierarchy, bento=content showcases with semantic asymmetry
 *
 * @usage-patterns
 * DO: Linear - Product catalogs, image galleries, equal-priority content
 * DO: Golden - Editorial layouts, feature showcases, natural hierarchy
 * DO: Bento - Dashboards, content showcases (use sparingly, high cognitive load)
 * DO: Limit items to 8 max on wide screens (Miller's Law)
 * NEVER: Decorative asymmetry without semantic meaning
 * NEVER: Exceed cognitive load limits
 *
 * @example
 * ```tsx
 * // Equal-priority grid
 * <Grid preset="linear" columns={3} gap="4">
 *   <Grid.Item>Card 1</Grid.Item>
 *   <Grid.Item>Card 2</Grid.Item>
 *   <Grid.Item>Card 3</Grid.Item>
 * </Grid>
 *
 * // Bento dashboard layout
 * <Grid preset="bento" pattern="dashboard">
 *   <Grid.Item priority="primary">Main Metric</Grid.Item>
 *   <Grid.Item priority="secondary">Chart</Grid.Item>
 * </Grid>
 * ```
 */
import * as React from 'react';
import { createBehavior, type PartIds } from '@/lib/contract';
import { useMemory } from '@/hooks/use-memory';
import classy from '@/lib/primitives/classy';
import { createRovingFocus } from '@/lib/primitives/roving-focus';
import {
  grid,
  gridItemAttrs,
  type ContentPriority,
  type GridConfig,
  type GridPart,
  type ResponsiveColumns,
} from '@/components/ui/grid.behavior';
import { gridClasses, gridColSpanClasses, gridRowSpanClasses } from '@/components/ui/grid.classes';

type FixedColumns = 1 | 2 | 3 | 4 | 5 | 6 | 7 | 8 | 9 | 10 | 11 | 12;

/**
 * The 12-column grid: named attention structures over a column vocabulary.
 * Linear/preset layouts are silent furniture; an honest `role="grid"` engages
 * the ARIA grid keyboard contract (2D roving).
 *
 * @cognitive-load 2/10 - decision 1, information 0, interaction 0 (layout
 * mode) or 3 (grid mode: arrow-key traversal is learned), disruption 0,
 * learning 1. As layout it is pure structure; as role="grid" it adds a
 * keyboard contract users must discover.
 * @attention-economics The placement channel IS the attention economy:
 * `priority` declares hierarchy and the stock layouts (golden, bento) spend
 * span on `primary`. At most one primary per grid -- two heroes is no hero.
 * @trust-building Structural, not persuasive. Reordering the source tree
 * never changes which item is the hero (placement keys off declared
 * priority, never source position), so the visual hierarchy is honest.
 * @accessibility Layout grids project NO role (1.3.1: structure is not
 * over-announced). role="grid" is type-gated to fixed columns + an
 * accessible name + row/gridcell structure (4.1.2) and supplies full
 * arrow/Home/End keyboard traversal (2.1.1).
 */
interface GridBaseProps extends Omit<React.HTMLAttributes<HTMLDivElement>, 'role'> {
  preset?: 'linear' | 'golden' | 'bento';
  pattern?: 'editorial' | 'dashboard' | 'feature' | 'portfolio';
  gap?: GridConfig['gap'];
  padding?: GridConfig['padding'];
}

/** role='grid' promises the ARIA grid pattern, so it demands what the
 *  pattern demands at the type level: a FIXED column count (fluid columns
 *  cannot honestly claim row structure), an accessible name, and the
 *  linear preset (uniform cells -- spans would break row chunking). */
type GridA11yProps =
  | { role?: 'presentation' | undefined; columns?: ResponsiveColumns; 'aria-label'?: string }
  | {
      role: 'grid';
      columns: FixedColumns;
      'aria-label': string;
      preset?: 'linear';
      pattern?: never;
    };

export type GridProps = GridBaseProps & GridA11yProps;

interface GridContextValue {
  interactive: boolean;
}

const GridContext = React.createContext<GridContextValue>({ interactive: false });

function GridRoot(props: GridProps) {
  const {
    preset = 'linear',
    pattern,
    columns,
    gap,
    padding,
    role,
    'aria-label': ariaLabel,
    className,
    children,
    ...rest
  } = props;

  const config: GridConfig = { preset, pattern, columns, gap, padding, role, ariaLabel };

  // The controller composes the score with the substrate -- no useBehavior.
  // Grid is a static score: createBehavior is a formality (state never moves),
  // useMemory subscribes React to it, and a useEffect below composes the
  // roving-focus primitive directly (present only under an honest role="grid").
  const { memory } = React.useMemo(() => createBehavior(grid, config), []);
  const state = useMemory(memory);
  const classes = gridClasses(config, state);
  const interactive = role === 'grid' && typeof columns === 'number';

  const rootRef = React.useRef<HTMLDivElement>(null);

  // role="grid" with a fixed column count engages the ARIA grid keyboard
  // contract: compose the roving-focus primitive directly with its 2D columns
  // option (Left/Right by 1, Up/Down by a row, Home/End to the ends). It owns
  // the roving tabindex across the [data-roving-item] cells. Presentation and
  // fluid-column grids stay inert -- the effect only runs when interactive.
  React.useEffect(() => {
    if (!interactive) return;
    const root = rootRef.current;
    if (!root) return;
    return createRovingFocus(root, { columns: columns as number });
  }, [interactive, columns]);

  // grid.aria ignores ids (the root carries no id refs); pass empties.
  const aria = grid.aria(state, config, { root: '', row: '', cell: '' } as PartIds<GridPart>);

  // ARIA grid structure: chunk children into role=row wrappers of exactly
  // `columns` cells. Mechanical -- geometry comes from config, roles from
  // the score's part declarations.
  const content = interactive
    ? chunk(React.Children.toArray(children), columns as number).map((row, rowIndex) => (
        // biome-ignore lint/suspicious/noArrayIndexKey: rows are positional by definition
        <div key={rowIndex} data-part="row" role="row" className="contents">
          {row.map((cell, cellIndex) => (
            <div
              // biome-ignore lint/suspicious/noArrayIndexKey: cells are positional by definition
              key={cellIndex}
              data-part="cell"
              role="gridcell"
              data-roving-item
              tabIndex={-1}
            >
              {cell}
            </div>
          ))}
        </div>
      ))
    : children;

  return (
    <GridContext.Provider value={{ interactive }}>
      <div
        data-part="root"
        ref={rootRef}
        className={classy(classes.root, className)}
        {...aria.root}
        {...rest}
      >
        {content}
      </div>
    </GridContext.Provider>
  );
}

function chunk<T>(items: T[], size: number): T[][] {
  const rows: T[][] = [];
  for (let index = 0; index < items.length; index += size) {
    rows.push(items.slice(index, index + size));
  }
  return rows;
}

export interface GridItemProps extends React.HTMLAttributes<HTMLDivElement> {
  /** The item DECLARES what it is; the stock layouts place it by this
   *  projection, never by source order. */
  priority?: ContentPriority;
  colSpan?: 1 | 2 | 3 | 4 | 5 | 6 | 7 | 8 | 9 | 10 | 11 | 12;
  rowSpan?: 1 | 2 | 3;
}

function GridItem({ priority, colSpan, rowSpan, className, ...props }: GridItemProps) {
  const classes = classy(
    colSpan && gridColSpanClasses[colSpan],
    rowSpan && gridRowSpanClasses[rowSpan],
    className,
  );

  return <div className={classes || undefined} {...gridItemAttrs(priority)} {...props} />;
}

GridRoot.displayName = 'Grid';
GridItem.displayName = 'GridItem';

export const Grid = Object.assign(GridRoot, {
  Item: GridItem,
});

export default Grid;
