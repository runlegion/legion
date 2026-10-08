/**
 * Progress indicator for task completion status
 *
 * @cognitive-load 4/10 - Moderate attention required for progress monitoring
 * @attention-economics Temporal attention: Holds user attention during wait states with clear progress indication
 * @trust-building Accurate progress builds user confidence; clear completion states and next steps
 * @accessibility Screen reader announcements via native progress element; keyboard navigation not applicable
 * @semantic-meaning Progress communication: determinate=known duration with value, indeterminate=unknown duration
 *
 * @usage-patterns
 * DO: Provide accurate progress indication when possible
 * DO: Use indeterminate for unknown durations
 * DO: Show clear completion states
 * DO: Include value labels for complex operations
 * NEVER: Fake progress (inaccurate progress bars)
 * NEVER: Use for instant operations (< 1 second)
 * NEVER: Leave progress at 99% indefinitely
 *
 * @example
 * ```tsx
 * // Determinate progress
 * <Progress value={66} />
 *
 * // With custom label
 * <Progress
 *   value={3}
 *   max={10}
 *   getValueLabel={(value, max) => `${value} of ${max} files uploaded`}
 * />
 *
 * // Indeterminate (loading)
 * <Progress />
 * ```
 */
import * as React from 'react';
import classy from '@/lib/primitives/classy';
import {
  progress,
  resolveProgress,
  type ProgressConfig,
  type ProgressSize,
  type ProgressVariant,
} from '@/components/ui/progress.behavior';
import { progressClasses } from '@/components/ui/progress.classes';

/**
 * Progress indicator for task completion status.
 *
 * A static score has nothing to subscribe to: this decorator is pure
 * decoration application. No useMemory, no effects -- config in, the ARIA
 * projection and classes out. The projection (`progress.aria`) and the fill
 * width (`resolveProgress`) are pure functions computed inline; React applies
 * them declaratively, the WC/Astro decorators apply them through bindProgress.
 *
 * shadcn-compatible base: `<Progress value={n} max={m} />`. Rafters
 * extensions: `variant`, `size`, `getValueLabel`.
 *
 * @cognitive-load 4/10 - decision 0, information 2 (read the fill), interaction
 * 0, disruption 1 (holds attention during a wait), learning 1. Moderate
 * attention required for progress monitoring; no interaction to learn.
 * @attention-economics Temporal attention: holds the user during a wait state
 * with an honest, monotonic fill. Indeterminate signals unknown duration
 * rather than faking a value.
 * @trust-building Accurate progress builds confidence; never fake the fill,
 * never park at 99%. Determinate value is honest, indeterminate is honest
 * about being unknown.
 * @accessibility role="progressbar" with aria-valuemin/max/now and aria-valuetext;
 * indeterminate omits aria-valuenow and sets aria-busy. Requires an accessible
 * name (aria-label / aria-labelledby). No keyboard interaction (not a widget).
 * @semantic-meaning Progress communication: determinate=known duration with a
 * value, indeterminate=unknown duration.
 *
 * @usage-patterns
 * DO: Provide accurate progress indication when possible
 * DO: Use indeterminate (omit value) for unknown durations
 * DO: Include value labels for complex operations via getValueLabel
 * NEVER: Fake progress or leave it at 99% indefinitely
 * NEVER: Use for instant operations (< 1 second)
 *
 * @example
 * ```tsx
 * <Progress value={66} aria-label="Upload progress" />
 * <Progress value={3} max={10} aria-label="Files"
 *   getValueLabel={(v, m) => `${v} of ${m} files`} />
 * <Progress aria-label="Loading" />
 * ```
 */

export interface ProgressProps extends React.HTMLAttributes<HTMLDivElement> {
  /** Current progress value (0 to max). Undefined = indeterminate. */
  value?: number;
  /** Maximum value. Default 100. */
  max?: number;
  /** Callback to generate the accessible value text. */
  getValueLabel?: (value: number, max: number) => string;
  variant?: ProgressVariant;
  size?: ProgressSize;
}

export const Progress = React.forwardRef<HTMLDivElement, ProgressProps>(function Progress(
  { value, max = 100, getValueLabel, variant = 'default', size = 'default', className, ...props },
  ref,
) {
  // getValueLabel is a rafters extension -- a framework affordance the score
  // cannot express as an attribute; feed its output into config.valueText so
  // the ONE projection formats the label.
  const effectiveMax = typeof max === 'number' && Number.isFinite(max) && max > 0 ? max : 100;
  const valueText =
    value !== undefined && Number.isFinite(value) && getValueLabel
      ? getValueLabel(Math.min(Math.max(value, 0), effectiveMax), effectiveMax)
      : undefined;

  const config: ProgressConfig = { value, max, valueText, variant, size };
  const classes = progressClasses(config, {});
  const aria = progress.aria({}, config, { root: '', indicator: '' });
  const { indeterminate, percent } = resolveProgress(config);

  return (
    <div
      ref={ref}
      data-part="root"
      className={classy(classes.root, className)}
      {...aria.root}
      {...props}
    >
      <div
        data-part="indicator"
        className={classes.indicator}
        style={indeterminate ? undefined : { width: `${percent}%` }}
        {...aria.indicator}
      />
    </div>
  );
});

Progress.displayName = 'Progress';

export default Progress;
