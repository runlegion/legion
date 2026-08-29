/**
 * Label -- a form control label. Associate it with a control through the
 * native `htmlFor` IDREF (`<Label htmlFor="email">` binds `<Input id="email">`);
 * the association is the platform's, not the score's. Nine semantic colour
 * variants ride the role vocabulary (rafters extension over the shadcn base).
 *
 * @cognitive-load 2/10 - decision 0, info 1, interaction 0, disruption 0, learning 1
 * @attention-economics A label names, it does not compete for attention. The
 * variant is an information-hierarchy channel, not decoration: use `default`
 * for a plain field label, `destructive` for an error-state label, `muted` for
 * a hint-weight label -- field, hint, and error are usage roles the same nine
 * colours express, not separate widgets. Reserve high-chroma variants for the
 * one label that must be read first, or the form flattens into noise.
 * @trust-building Clear, honest requirement indication and non-punitive
 * error-state colour; a label that names its control predictably (never an
 * orphaned or ambiguous label) is the smallest unit of form trust.
 * @accessibility The association is native `htmlFor`/`for` (an IDREF to the
 * control's id), so a screen reader announces the label when the control takes
 * focus. Colour variants are never the sole carrier of meaning -- an error
 * label pairs its colour with the control's own `aria-invalid`/error text.
 * @semantic-meaning Variant is a colour channel over the role vocabulary:
 * default = field label, destructive = validation error, muted = supplemental
 * hint, success = confirmation. The variant styles the text; it never changes
 * the label's role or its association to a control.
 *
 * @usage-patterns
 * DO: Always associate with a control via htmlFor/id (or by wrapping it).
 * DO: Use the variant to guide attention to the label that matters most.
 * NEVER: Orphaned labels, ambiguous text, or colour as the only error signal.
 *
 * A pure static score has nothing to subscribe to: the performance is pure
 * decoration application. No useBehavior, no memory, no bind -- config in,
 * classes out, children through, `htmlFor` passed to the native element.
 *
 * @example
 * ```tsx
 * <Label htmlFor="email">Email address</Label>
 * <Input id="email" type="email" />
 *
 * <Label htmlFor="name" variant="destructive">Name is required</Label>
 * ```
 */
import * as React from "react";
import classy from "@/lib/primitives/classy";
import type { LabelVariant } from "@/components/ui/label.behavior";
import { labelClasses } from "@/components/ui/label.classes";

export interface LabelProps extends React.LabelHTMLAttributes<HTMLLabelElement> {
  variant?: LabelVariant;
}

export const Label = React.forwardRef<HTMLLabelElement, LabelProps>(
  ({ className, variant = "default", ...props }, ref) => {
    const classes = labelClasses({ variant }, {});
    return (
      // biome-ignore lint/a11y/noLabelWithoutControl: Label associates with a control via the consumer's htmlFor prop or by wrapping the control
      <label
        ref={ref}
        data-part="root"
        className={classy(classes.root, className) || undefined}
        {...props}
      />
    );
  },
);

Label.displayName = "Label";

export default Label;
