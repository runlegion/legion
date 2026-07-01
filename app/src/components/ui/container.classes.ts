/**
 * Shared class definitions for Container component
 * Used by both container.tsx (React) and container.astro (Astro)
 */

export const containerSizeClasses: Record<string, string> = {
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

export const containerPaddingClasses: Record<string, string> = {
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

export const containerGapClasses: Record<string, string> = {
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

export const containerSizeGapScale: Record<string, string> = {
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

/**
 * Container-query activation utilities. Turns the element into a query
 * container; w-full prevents width collapse in flex/grid contexts under
 * Tailwind v4. Always applied by the React/Astro targets when query is on.
 */
export const containerQueryClasses = '@container w-full';

/**
 * Horizontal centering for sized (non-full) containers.
 */
export const containerCenterClasses = 'mx-auto';

/**
 * Editable-mode affordance: dashed outline in muted-foreground at 30% with a
 * small offset and rounded corner. Mirrors the inline editable composition in
 * container.tsx.
 */
export const containerEditableClasses =
  'outline-2 outline-dashed outline-muted-foreground/30 outline-offset-2 rounded';

/**
 * CQ-responsive horizontal padding applied automatically when size is set.
 * Prevents content from bleeding to viewport edges without requiring an explicit padding prop.
 * Explicit padding prop overrides this.
 */
export const containerAutoEdgePadding = 'px-4 @md:px-6 @lg:px-8';

export const containerArticleTypography = [
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

export type ContainerPosition = 'sticky' | 'fixed' | 'relative' | 'absolute' | 'static';

export const containerPositionClasses: Record<ContainerPosition, string> = {
  sticky: 'sticky top-0',
  fixed: 'fixed top-0 left-0 right-0',
  relative: 'relative',
  absolute: 'absolute',
  static: 'static',
};

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

export const containerDepthClasses: Record<ContainerDepth, string> = {
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

export type ContainerBackground = 'none' | 'muted' | 'accent' | 'card' | 'primary';

export const containerBackgroundClasses: Record<ContainerBackground, string> = {
  none: '',
  muted: 'bg-muted',
  accent: 'bg-accent',
  card: 'bg-card',
  primary: 'bg-primary text-primary-foreground',
};
