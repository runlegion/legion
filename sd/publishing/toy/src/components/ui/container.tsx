/**
 * Semantic container component for layout structure and content boundaries
 *
 * @cognitive-load 0/10 - Invisible structure that reduces visual complexity
 * @attention-economics Neutral structural element: Controls content width and breathing room without competing for attention
 * @trust-building Predictable boundaries and consistent spacing patterns
 * @accessibility Semantic HTML elements with proper landmark roles for screen readers
 * @semantic-meaning Element-driven behavior: main=primary landmark, header=page/section header, footer=page/section footer, section=structural grouping, article=readable content with typography, aside=supplementary, div=no semantics
 *
 * @usage-patterns
 * DO: Use main for the primary content area (once per page)
 * DO: Use header with position="sticky" and depth="navigation" for site headers
 * DO: Use footer for page or section footers
 * DO: Use section for structural grouping within grids
 * DO: Use article for readable content - typography is automatic
 * DO: Use aside for supplementary content, add surface classes for depth
 * DO: Spacing happens inside (padding), not outside (no margins)
 * DO: Place a Container directly in a Grid and give it colSpan/rowSpan -- no Grid.Item wrapper needed
 * NEVER: Nest containers unnecessarily
 * NEVER: Use margins for spacing - let parent Grid handle gaps
 * NEVER: Use @container without w-full in flex/grid contexts (causes width collapse in Tailwind v4)
 * NEVER: Write raw sticky/fixed/z-* utilities -- use position and depth props
 *
 * @example
 * ```tsx
 * <Container as="header" position="sticky" depth="navigation" fill="background" size="full" padding="4">
 *   <nav>Site navigation</nav>
 * </Container>
 * <Container as="main" size="6xl" padding="6">
 *   <Container as="article">
 *     <h1>Title</h1>
 *     <p>Typography just works.</p>
 *   </Container>
 * </Container>
 * <Container as="footer" padding="6">
 *   <p>Footer content</p>
 * </Container>
 *
 * // Self-placing grid children -- no Grid.Item wrappers
 * <Grid columns={3} gap="6">
 *   <Container as="article" size="5xl" colSpan={2} queryName="main">…</Container>
 *   <Container as="aside" colSpan={1} queryName="rail">…</Container>
 * </Grid>
 * ```
 */
import * as React from 'react';
import classy from '@/lib/primitives/classy';
import type {
  ContainerConfig,
  ContainerDepth,
  ContainerElement,
  ContainerPadding,
  ContainerPosition,
  ContainerSize,
} from '@/components/ui/container.behavior';
import { containerClasses } from '@/components/ui/container.classes';
import type { ResponsiveColumns } from '@/components/ui/grid.behavior';

/**
 * A static score has nothing to subscribe to: the performance is pure
 * decoration application. No useBehavior, no memory -- config in, classes
 * out, semantic element chosen by `as`.
 */

export interface ContainerProps extends React.HTMLAttributes<HTMLElement> {
  as?: ContainerElement;
  size?: ContainerSize;
  padding?: ContainerPadding;
  gap?: boolean | ContainerPadding;
  /** Grid mode: define a container and a grid in one tag (linear columns,
   *  1-12 or responsive; presets stay Grid's). */
  columns?: ResponsiveColumns;
  query?: boolean;
  queryName?: string;
  colSpan?: 1 | 2 | 3 | 4 | 5 | 6 | 7 | 8 | 9 | 10 | 11 | 12;
  rowSpan?: 1 | 2 | 3;
  position?: ContainerPosition;
  depth?: ContainerDepth;
  fill?: string;
}

export const Container = React.forwardRef<HTMLElement, ContainerProps>(
  (
    {
      as: Element = 'div',
      size,
      padding,
      gap,
      columns,
      query = true,
      queryName,
      colSpan,
      rowSpan,
      position,
      depth,
      fill,
      className,
      style,
      children,
      ...props
    },
    ref,
  ) => {
    const config: ContainerConfig = {
      as: Element,
      size,
      padding,
      gap,
      columns,
      query,
      queryName,
      colSpan,
      rowSpan,
      position,
      depth,
      fill,
    };

    const classes = containerClasses(config, {});

    // containerName cannot be a literal class (arbitrary values are
    // banned); the one style channel, ruled narrowly for CQ naming.
    const containerStyle: React.CSSProperties = {
      ...style,
      ...(queryName && { containerName: queryName }),
    };

    return React.createElement(
      Element,
      {
        ref,
        'data-part': 'root',
        className: classy(classes.root, className) || undefined,
        style: Object.keys(containerStyle).length > 0 ? containerStyle : undefined,
        'data-fill': fill || undefined,
        ...props,
      },
      children,
    );
  },
);

Container.displayName = 'Container';

export default Container;
