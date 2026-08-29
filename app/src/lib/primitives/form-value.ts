/**
 * form-value.ts - framework-agnostic form association for custom controls.
 *
 * A custom control (a select built from a button + listbox, a slider built
 * from a div, ...) is not a native form field, so a plain `<form>` submit
 * skips it. The lowest-common-denominator fix that works in every framework
 * and every browser is a mirrored hidden `<input>`: it carries the control's
 * `name`/`value` into the form's submission set with zero JS at submit time.
 *
 * This primitive is a PURE attrs builder (kind: pure, like dialog-aria): it
 * describes the hidden input the three decorators render, and never touches
 * the DOM itself. The behavior's bind keeps the input's value in sync as the
 * control's value moves; there is nothing stateful to own here.
 *
 * @example
 * ```ts
 * const attrs = formValueAttrs({ name: 'fruit', value: 'apple' });
 * // -> { type: 'hidden', name: 'fruit', value: 'apple' }
 * // A control with no `name` submits nothing:
 * formValueAttrs({ value: 'apple' }); // -> null
 * ```
 */

export interface FormValueConfig {
  /** The form field name. Without it the control is not form-associated. */
  name?: string | undefined;
  /** The current value carried into the form. */
  value?: string | undefined;
}

export interface FormValueAttrs {
  type: "hidden";
  name: string;
  value: string;
}

/** Whether the control participates in form submission (has a `name`). */
export function isFormAssociated(config: FormValueConfig): boolean {
  return typeof config.name === "string" && config.name.length > 0;
}

/**
 * Build the hidden input's attributes, or `null` when the control has no
 * `name` (nothing to submit). Value defaults to the empty string so an
 * unselected control still submits a present-but-empty field.
 */
export function formValueAttrs(config: FormValueConfig): FormValueAttrs | null {
  if (!isFormAssociated(config)) return null;
  return { type: "hidden", name: config.name as string, value: config.value ?? "" };
}
