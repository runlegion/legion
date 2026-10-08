import type { SeparatorConfig, SeparatorOrientation, SeparatorState } from '@/components/ui/separator.behavior';

export interface SeparatorClassSet {
  root: string;
}

/** Structural base: never shrinks in a flex context, filled with the border
 *  token. `fill`, not a raw background -- `bg-border` is the semantic divider
 *  colour, ported verbatim from the oracle. */
const baseClasses = 'shrink-0 bg-border';

/** The rule's thin axis: a 1px line the full length of the other axis. */
const orientationClasses: Record<SeparatorOrientation, string> = {
  horizontal: 'h-px w-full',
  vertical: 'h-full w-px',
};

export function separatorClasses(
  config: SeparatorConfig,
  _state: SeparatorState,
): SeparatorClassSet {
  const orientation = config.orientation ?? 'horizontal';
  return { root: `${baseClasses} ${orientationClasses[orientation]}` };
}
