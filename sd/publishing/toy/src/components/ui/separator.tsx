/**
 * Visual separator component for dividing content sections
 *
 * @cognitive-load 0/10 - Passive visual element, no cognitive processing required
 * @attention-economics Neutral structure: creates visual boundaries without demanding attention
 * @trust-building Consistent spacing, clear content grouping, predictable visual hierarchy
 * @accessibility role="separator" or role="none" for decorative, orientation for screen readers
 * @semantic-meaning Visual division: horizontal=between sections, vertical=between inline items
 *
 * @usage-patterns
 * DO: Use to group related content visually
 * DO: Use horizontal for section breaks
 * DO: Use vertical for inline item separation (toolbars, menus)
 * DO: Set decorative=true when purely visual
 * NEVER: Overuse separators, use when whitespace alone suffices
 *
 * @example
 * ```tsx
 * // Horizontal section divider
 * <Separator />
 *
 * // Vertical toolbar divider
 * <Separator orientation="vertical" className="h-4" />
 * ```
 */
import * as React from 'react';
import classy from '@/lib/primitives/classy';
import { separator } from '@/components/ui/separator.behavior';
import type { SeparatorConfig, SeparatorOrientation } from '@/components/ui/separator.behavior';
import { separatorClasses } from '@/components/ui/separator.classes';

/**
 * Visual divider. A horizontal or vertical rule; decorative by default
 * (`role="none"`), semantic on request (`role="separator"`).
 *
 * @cognitive-load 0/10 - decision 0, information 0, interaction 0,
 * disruption 0, learning 0. A passive visual boundary: nothing to read,
 * decide, operate, or learn. It structures other content without adding
 * cognitive cost of its own.
 * @attention-economics Neutral structure. A separator creates visual
 * boundaries without demanding attention; it guides the eye between groups
 * and never competes with the content it divides. Overusing rules where
 * whitespace alone would group is the only attention cost.
 * @trust-building Consistent, predictable grouping. A rule where a section
 * ends builds a reliable visual hierarchy; decorative-by-default keeps
 * meaningless boundaries out of the accessibility tree so assistive
 * technology is never told about a line that carries no meaning.
 * @accessibility Decorative rules project `role="none"` (removed from the
 * a11y tree); semantic rules project `role="separator"` with
 * `aria-orientation`, announced as a section break. The role and orientation
 * are the entire accessible contract, resolved by the score.
 */
export interface SeparatorProps extends React.HTMLAttributes<HTMLDivElement> {
  /** Rule axis. Default `horizontal`. */
  orientation?: SeparatorOrientation;
  /** Purely visual (removed from the a11y tree). Default `true`. */
  decorative?: boolean;
}

export const Separator = React.forwardRef<HTMLDivElement, SeparatorProps>(
  ({ className, orientation = 'horizontal', decorative = true, ...props }, ref) => {
    const config: SeparatorConfig = { orientation, decorative };
    const classes = separatorClasses(config, {});

    // The role/orientation decision lives in the score; the performance only
    // paints the resolved projection. A static projection means no bind.
    const projection = separator.aria({}, config, { root: '' }).root ?? {};
    const role = projection.role as React.AriaRole | undefined;
    const ariaOrientation = projection['aria-orientation'] as SeparatorOrientation | undefined;

    return (
      <div
        ref={ref}
        data-part="root"
        role={role}
        aria-orientation={ariaOrientation}
        className={classy(classes.root, className) || undefined}
        {...props}
      />
    );
  },
);

Separator.displayName = 'Separator';

export default Separator;
