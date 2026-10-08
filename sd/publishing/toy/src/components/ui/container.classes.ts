import { resolveFillName } from '@/lib/primitives/fill-resolver';
import {
  gridColSpanClasses,
  gridGapClasses,
  gridRowSpanClasses,
  resolveColumnsClasses,
} from '@/components/ui/grid.classes';
import type {
  ContainerConfig,
  ContainerDepth,
  ContainerPosition,
  ContainerState,
} from '@/components/ui/container.behavior';

export interface ContainerClassSet {
  root: string;
}

const sizeClasses: Record<string, string> = {
  sm: 'max-w-sm',
  md: 'max-w-md',
  lg: 'max-w-lg',
  xl: 'max-w-xl',
  '2xl': 'max-w-2xl',
  '3xl': 'max-w-3xl',
  '4xl': 'max-w-4xl',
  '5xl': 'max-w-5xl',
  '6xl': 'max-w-6xl',
  '7xl': 'max-w-7xl',
  full: 'w-full',
};

const paddingClasses: Record<string, string> = {
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
  '16': 'p-16',
  '20': 'p-20',
  '24': 'p-24',
};

/** Stack mode: vertical flow with gap. */
const stackGapClasses: Record<string, string> = {
  '0': 'flex flex-col gap-0',
  '1': 'flex flex-col gap-1',
  '2': 'flex flex-col gap-2',
  '3': 'flex flex-col gap-3',
  '4': 'flex flex-col gap-4',
  '5': 'flex flex-col gap-5',
  '6': 'flex flex-col gap-6',
  '8': 'flex flex-col gap-8',
  '10': 'flex flex-col gap-10',
  '12': 'flex flex-col gap-12',
  '16': 'flex flex-col gap-16',
  '20': 'flex flex-col gap-20',
  '24': 'flex flex-col gap-24',
};

/** gap=true walks the spacing scale by size: component tier into section tier. */
const sizeGapScale: Record<string, string> = {
  sm: '3',
  md: '4',
  lg: '5',
  xl: '6',
  '2xl': '6',
  '3xl': '8',
  '4xl': '8',
  '5xl': '10',
  '6xl': '10',
  '7xl': '12',
};

/** CQ provider; w-full prevents width collapse in flex/grid contexts (TW v4). */
const queryClasses = '@container w-full';

/** CQ-responsive edge padding, applied when sized and no explicit padding. */
const autoEdgePadding = 'px-4 @md:px-6 @lg:px-8';

const positionClasses: Record<ContainerPosition, string> = {
  sticky: 'sticky top-0',
  fixed: 'fixed top-0 left-0 right-0',
  relative: 'relative',
  absolute: 'absolute',
  static: 'static',
};

const depthClasses: Record<ContainerDepth, string> = {
  base: 'z-depth-base',
  dropdown: 'z-depth-dropdown',
  sticky: 'z-depth-sticky',
  navigation: 'z-depth-navigation',
  fixed: 'z-depth-fixed',
  modal: 'z-depth-modal',
  popover: 'z-depth-popover',
  tooltip: 'z-depth-tooltip',
  overlay: 'z-depth-overlay',
  below: 'z-depth-below',
  max: 'z-depth-max',
};

/**
 * Article typography flow. Ported verbatim from the oracle: raw sizes,
 * NOT the typography role utilities -- repointing these at the display,
 * title, and body role tokens is a DESIGNER pass, flagged in the spec,
 * not an agent decision.
 */
const articleTypography = [
  '[&_p]:leading-relaxed',
  '[&_p]:mb-4',
  '[&_p:last-child]:mb-0',
  '[&_h1]:text-4xl [&_h1]:font-bold [&_h1]:tracking-tight [&_h1]:mb-4 [&_h1]:mt-0 [&_h1]:text-accent-foreground',
  '[&_h2]:text-3xl [&_h2]:font-semibold [&_h2]:tracking-tight [&_h2]:mb-3 [&_h2]:mt-8 [&_h2]:first:mt-0 [&_h2]:text-accent-foreground',
  '[&_h3]:text-2xl [&_h3]:font-semibold [&_h3]:mb-2 [&_h3]:mt-6 [&_h3]:text-accent-foreground',
  '[&_h4]:text-xl [&_h4]:font-semibold [&_h4]:mb-2 [&_h4]:mt-4 [&_h4]:text-accent-foreground',
  '[&_ul]:list-disc [&_ul]:pl-6 [&_ul]:mb-4',
  '[&_ol]:list-decimal [&_ol]:pl-6 [&_ol]:mb-4',
  '[&_li]:mb-1',
  '[&_a]:text-primary [&_a]:underline [&_a]:underline-offset-4 [&_a:hover]:text-primary/80',
  '[&_blockquote]:border-l-4 [&_blockquote]:border-muted [&_blockquote]:pl-4 [&_blockquote]:italic [&_blockquote]:my-4',
  '[&_code]:bg-muted [&_code]:px-1.5 [&_code]:py-0.5 [&_code]:rounded [&_code]:text-sm [&_code]:font-mono',
  '[&_pre]:bg-muted [&_pre]:p-4 [&_pre]:rounded-lg [&_pre]:overflow-x-auto [&_pre]:my-4',
  '[&_pre_code]:bg-transparent [&_pre_code]:p-0',
  '[&_hr]:border-border [&_hr]:my-8',
  '[&_img]:rounded-lg [&_img]:my-4',
  '[&_table]:w-full [&_table]:my-4',
  '[&_th]:border [&_th]:border-border [&_th]:px-3 [&_th]:py-2 [&_th]:text-left [&_th]:font-semibold',
  '[&_td]:border [&_td]:border-border [&_td]:px-3 [&_td]:py-2',
  'max-w-prose',
].join(' ');

export function containerClasses(
  config: ContainerConfig,
  _state: ContainerState,
): ContainerClassSet {
  const parts: string[] = [];

  if (config.query !== false) parts.push(queryClasses);

  if (config.size) {
    parts.push(sizeClasses[config.size] ?? '');
    if (config.size !== 'full') parts.push('mx-auto');
  }

  if (config.padding) {
    parts.push(paddingClasses[config.padding] ?? '');
  } else if (config.size && config.size !== 'full') {
    parts.push(autoEdgePadding);
  }

  // Layout mode is exclusive: columns -> grid; gap alone -> stack.
  if (config.columns !== undefined) {
    parts.push('grid', resolveColumnsClasses(config.columns));
    const gridGap =
      config.gap === true
        ? (config.size && sizeGapScale[config.size]) || '6'
        : typeof config.gap === 'string'
          ? config.gap
          : undefined;
    if (gridGap) parts.push(gridGapClasses[gridGap] ?? '');
  } else if (config.gap) {
    const stackGap =
      config.gap === true ? (config.size && sizeGapScale[config.size]) || '6' : config.gap;
    parts.push(stackGapClasses[stackGap] ?? '');
  }

  if (config.colSpan) parts.push(gridColSpanClasses[config.colSpan] ?? '');
  if (config.rowSpan) parts.push(gridRowSpanClasses[config.rowSpan] ?? '');

  if (config.fill) parts.push(resolveFillName(config.fill, 'surface'));

  if (config.position) parts.push(positionClasses[config.position]);
  if (config.depth) parts.push(depthClasses[config.depth]);

  if (config.as === 'article') parts.push(articleTypography);

  return { root: parts.filter(Boolean).join(' ') };
}
