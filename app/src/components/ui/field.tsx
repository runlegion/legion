/**
 * Field -- the React performance of the field score. The shadcn Field surface:
 * a container that pairs a label with a slotted control and optional
 * helper/error text, and wires the id-association + validity ARIA the score
 * projects. React reads the projection declaratively (no bind): it clones the
 * control child to inject the id and `fieldControlAria`, renders a `<label>`
 * with the native `for` association (using the Label score's own decoration),
 * and paints the required marker + disabled dim from field.classes.ts.
 *
 * @cognitive-load 3/10 - decision 0, info 2, interaction 0, disruption 0, learning 1
 * @attention-economics Information hierarchy, not competition: label = field
 * identity, control = the action area, description = quiet guidance, error =
 * the one part that earns attention. The error's colour + `role="alert"` are
 * reserved for a real validation failure so a field at rest stays calm.
 * @trust-building Predictable labelling removes uncertainty; a description sets
 * expectations before the user acts; error messaging is specific and
 * non-punitive (it names what to fix, styled with the destructive token but
 * never shouting). A field that always names its control is the smallest unit
 * of form trust.
 * @accessibility Native label<->control association (`for`/`id`), the control's
 * `aria-describedby` composed from the rendered error + description ids (error
 * first, never dangling), `aria-invalid` while an error is shown,
 * `aria-required` while required, `disabled` propagated to the control, and
 * `role="alert"` on the error so a screen reader announces it. The required
 * marker is `aria-hidden` -- the requirement reaches AT through `aria-required`,
 * not the visual asterisk.
 * @semantic-meaning Field states: default = ready, error = validation failed
 * (description yields to the error), disabled = unavailable.
 *
 * @usage-patterns
 * DO: Always provide a label for the control.
 * DO: Use description for format hints or requirements.
 * DO: Use error with a clear, actionable message.
 * NEVER: Leave the control without an associated label.
 * NEVER: Use error styling without an error message.
 *
 * @example
 * ```tsx
 * <Field label="Email" description="We'll never share your email">
 *   <Input type="email" />
 * </Field>
 *
 * <Field label="Password" error="Password must be at least 8 characters">
 *   <Input type="password" />
 * </Field>
 * ```
 */
import * as React from "react";
import classy from "@/lib/primitives/classy";
import {
  descriptionId as deriveDescriptionId,
  errorId as deriveErrorId,
  fieldControlAria,
  type FieldConfig,
} from "@/components/ui/field.behavior";
import { composeFieldLabelClasses, fieldClassSet } from "@/components/ui/field.classes";

export interface FieldProps extends React.HTMLAttributes<HTMLDivElement> {
  /** Field label text. */
  label: React.ReactNode;
  /** Optional description/hint shown below the control (hidden while an error
   *  is present). */
  description?: React.ReactNode;
  /** Error message; when present the field enters its error state. */
  error?: React.ReactNode;
  /** Whether the field is required. */
  required?: boolean;
  /** Whether the field is disabled. */
  disabled?: boolean;
  /** Custom id connecting label, control, and helper/error ids. */
  id?: string;
  /** The form control (Input, Select, Textarea, ...). */
  children: React.ReactNode;
}

function useFieldId(providedId?: string): string {
  const reactId = React.useId();
  return providedId ?? `field-${reactId}`;
}

/** The id the first control child carries, if any -- so the label's `for`
 *  tracks an author-supplied control id (the earned WC semantic, upheld here). */
function firstChildId(children: React.ReactNode): string | undefined {
  for (const child of React.Children.toArray(children)) {
    if (React.isValidElement(child)) {
      const id = (child.props as Record<string, unknown>)["id"];
      if (typeof id === "string") return id;
    }
  }
  return undefined;
}

export const Field = React.forwardRef<HTMLDivElement, FieldProps>(function Field(
  { className, label, description, error, required, disabled, id: providedId, children, ...props },
  ref,
) {
  const fieldId = useFieldId(providedId);
  const controlId = firstChildId(children) ?? fieldId;

  const hasError = error != null && error !== false && error !== "";
  const hasDescription = description != null && description !== false && description !== "";
  // The description yields to the error: only one helper node is ever rendered.
  const showDescription = hasDescription && !hasError;

  const config: FieldConfig = { required, disabled };
  // The control's ARIA comes straight from the score, so the decorator and the
  // conformance harness agree by construction.
  const controlAria = fieldControlAria(fieldId, config, hasError, showDescription);

  const classes = fieldClassSet(config, {});

  const enhancedChildren = React.Children.map(children, (child) => {
    if (!React.isValidElement(child)) return child;
    const childProps = child.props as Record<string, unknown>;
    return React.cloneElement(child as React.ReactElement<Record<string, unknown>>, {
      "data-part": (childProps["data-part"] as string | undefined) ?? "control",
      // Author-supplied control id wins (the label tracks it); the field owns
      // the validity wiring, so the score's projection is authoritative there
      // -- no framework respects an author describedby, which keeps the three
      // performances drift-free (the score carries the contract).
      id: (childProps["id"] as string | undefined) ?? controlId,
      "aria-describedby": controlAria["aria-describedby"],
      "aria-invalid": controlAria["aria-invalid"],
      "aria-required": controlAria["aria-required"],
      disabled: (childProps["disabled"] as boolean | undefined) ?? disabled,
    });
  });

  return (
    <div ref={ref} data-part="root" className={classy(classes.container, className)} {...props}>
      {/* biome-ignore lint/a11y/noLabelWithoutControl: associated with the slotted control via htmlFor */}
      <label
        data-part="label"
        htmlFor={controlId}
        className={composeFieldLabelClasses(disabled === true)}
      >
        {label}
        {/* Decorative marker (aria-hidden): the requirement reaches AT through
            aria-required, never this glyph. */}
        {required && (
          <b className={classes.requiredMarker} aria-hidden="true">
            *
          </b>
        )}
      </label>

      {enhancedChildren}

      {showDescription && (
        <div
          data-part="description"
          id={deriveDescriptionId(fieldId)}
          className={classes.description}
        >
          {description}
        </div>
      )}

      {hasError && (
        <div data-part="error" id={deriveErrorId(fieldId)} className={classes.error} role="alert">
          {error}
        </div>
      )}
    </div>
  );
});

Field.displayName = "Field";

export default Field;
