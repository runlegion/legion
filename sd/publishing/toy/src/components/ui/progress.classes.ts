import {
  resolveProgress,
  type ProgressConfig,
  type ProgressSize,
  type ProgressState,
  type ProgressVariant,
} from '@/components/ui/progress.behavior';

/**
 * The view: class strings, no logic. The track (root) surface and the
 * indicator fill. Fill, not background -- the variant maps pick a `bg-*` role
 * token for the indicator, never a raw colour. The three performances all
 * compose their className through progressClasses so there is zero drift.
 */

export const progressContainerClasses = 'relative w-full overflow-hidden rounded-full bg-muted';

/**
 * `progress / fill / value change` in
 * `packages/ui/docs/spec/matrix/motion.jsonl` assigns tier `moderate` and
 * curve role `standard` (provenance `proposed`) to a `fill` whose declared
 * property is `inline-size / width`. A fill on a part that stays mounted is a
 * TRANSITION, named as composed generics; `ease-standard` was the half of the
 * row this file had not yet named.
 *
 * NARROWED FROM `transition-all` TO THE PROPERTY THE ROW NAMES. The
 * performances set `style.width` (progress.tsx, and `bindProgress` in
 * progress.behavior.ts), so `transition-[width]` is the fill and nothing else.
 * `transition-all` also timed the variant's `bg-*` colour, which no row
 * assigns -- motion the matrix never granted. The property name is not a
 * value; the duration and easing still come from the tokens.
 *
 * NO component-level reduced-motion escape, on the same law the indeterminate
 * loop below cites: the generated `duration-*` and `delay-*` utilities zero
 * themselves under `prefers-reduced-motion` (the exporter's
 * `REDUCED_MOTION_ZEROED` set), so reduced motion is the token sheet's
 * responsibility and never a component-level media query. This is the
 * transition half of the same rule -- the fill is a tier-kind moment, so
 * unlike the loop it DOES zero, and it zeroes on the leaf.
 */
export const progressIndicatorBaseClasses =
  'h-full transition-[width] duration-moderate ease-standard';

/**
 * THE CELL IS THE SPEC. `animate-pulse-shimmer` is the generated consumption of
 * `progress / root / indeterminate` -- keyframe `pulse` on period `shimmer`,
 * a LOOP, which is why it names a period instead of a tier and no curve (the
 * row declares `curve: none`).
 *
 * WHAT THIS REPLACES WAS DEAD. `animate-progress-indeterminate` resolved to
 * nothing: no `@utility` block, no `--animate-*` theme key, no keyframes
 * anywhere in the compiled sheet. It compiled, it painted nothing, and the
 * indeterminate bar has not animated for as long as that name has been here --
 * the silent-resolution failure the cell utilities exist to end.
 *
 * NO motion-reduce:animate-none. A period-kind cell is exempt from the
 * reduced-motion zeroing law by design (#2155, and `REDUCED_MOTION_ZEROED` in
 * `packages/design-tokens/src/exporters/tailwind.ts`): a stopped work loop says
 * the work stopped, which is false while the operation is still running.
 *
 * PART GAP, reported not resolved: the row names the part `root`, and this
 * class rides the INDICATOR. The root is the muted track, and pulsing the
 * track rather than the fill would be a visual change nobody assigned; while
 * indeterminate the indicator has no width set, so it spans the track and is
 * the surface that reads as the work.
 *
 * The row is marked `proposed` -- a starting position, never reviewed.
 */
export const progressIndeterminateClasses = 'animate-pulse-shimmer';

export const progressVariantClasses: Record<ProgressVariant, string> = {
  default: 'bg-primary',
  primary: 'bg-primary',
  secondary: 'bg-secondary',
  destructive: 'bg-destructive',
  success: 'bg-success',
  warning: 'bg-warning',
  info: 'bg-info',
  accent: 'bg-accent',
};

export const progressSizeClasses: Record<ProgressSize, string> = {
  sm: 'h-1',
  default: 'h-2',
  lg: 'h-3',
};

export interface ProgressClassSet {
  root: string;
  indicator: string;
}

export function progressClasses(config: ProgressConfig, _state: ProgressState): ProgressClassSet {
  const size = config.size ?? 'default';
  const variant = config.variant ?? 'default';
  const { indeterminate } = resolveProgress(config);

  const root = [progressContainerClasses, progressSizeClasses[size]].filter(Boolean).join(' ');
  const indicator = [
    progressIndicatorBaseClasses,
    progressVariantClasses[variant],
    indeterminate ? progressIndeterminateClasses : '',
  ]
    .filter(Boolean)
    .join(' ');

  return { root, indicator };
}
