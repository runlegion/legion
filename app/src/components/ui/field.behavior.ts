import { compose, type Slice } from "@/lib/compose";
import { type AriaAttrs, type BehaviorSpec, type PartIds } from "@/lib/contract";
import { updateAriaAttribute } from "@/lib/primitives/aria-manager";

/**
 * Field: the id-association / ARIA archetype. A form field wraps a native
 * control (Input, Select, Textarea, ...) with a Label and optional
 * helper/error text, and its whole job is WIRING -- associating the label with
 * the control and projecting validity/description ARIA onto the control. There
 * is no state axis, no reducer, no keymap: the score is a total function from
 * config (+ the rendered part ids) to the control's ARIA. That makes it a
 * static-with-projection score, the label archetype plus a non-empty
 * projection.
 *
 * What the score owns (the earned semantics, ported from src/old/ui/field.*):
 *  - the id-association scheme: label `for` <-> control `id`, with
 *    `${id}-description` and `${id}-error` as the sibling ids;
 *  - the `aria-describedby` composition: error id first, then description id,
 *    referencing ONLY ids that are actually rendered (the score derives
 *    presence from the ids the harness reads off the DOM, so a reference can
 *    never dangle -- the empty-id convention, ratified 2026-07-08);
 *  - `aria-invalid` on the control while an error message is present;
 *  - `aria-required` on the control while the field is required.
 *
 * What the score does NOT own (decorator/bind concerns, not ARIA):
 *  - the native `for`/`id` association wiring (bindField sets it, because the
 *    control is a slotted child SSR cannot reach);
 *  - `disabled` propagation to the control (a native attribute + property, not
 *    an ARIA projection -- mirrors input, whose disabled is a native prop);
 *  - the required marker (`aria-hidden` span) and the disabled label dimming --
 *    both are view/markup, painted by the decorators from field.classes.ts;
 *  - `role="alert"` on the error message -- a static PartDecl role.
 *
 * description-hidden-while-error: the decorators render the description node
 * only when there is no error (React/Astro enforce it in markup). Because the
 * score reads presence from rendered ids, a hidden description contributes no
 * id and is never referenced -- the two rules reconcile with no dangle.
 */
export interface FieldConfig {
  /** Advertised to AT via `aria-required` on the control, and drives the
   *  required marker in the view. */
  required?: boolean | undefined;
  /** Propagated to the control as the native `disabled` attribute/property, and
   *  dims the label in the view. Not an ARIA projection. */
  disabled?: boolean | undefined;
}

export type FieldState = Record<never, never>;
export type FieldActions = Record<never, never>;

/**
 * label   -- the `<label>`; its `for` association is native, so it carries no
 *            ARIA projection (the label archetype).
 * control -- the slotted form control the field wires ARIA onto.
 * description -- optional helper text; an `aria-describedby` id target only.
 * error   -- optional error message; `role="alert"` and an id target only.
 */
export type FieldPart = "label" | "control" | "description" | "error";

const field: Slice<FieldConfig, FieldState, FieldActions, FieldPart> = {
  name: "field",
  parts: {
    label: {},
    control: {},
    description: { optional: true },
    // role="alert" is the error message's live-region contract, asserted as a
    // PartDecl role across all three frameworks.
    error: { role: "alert", optional: true },
  },
  initialState: () => ({}),
  actions: {},
  canDispatch: () => true,
  aria: (_state, config, ids) => {
    // Presence is read from the rendered ids (behaviors never generate ids),
    // so a projected reference always points at a real element -- never a
    // dangling IDREF (an axe `aria-valid-attr-value` violation).
    const hasError = ids.error !== "";
    const hasDescription = ids.description !== "";
    // Error id first, then description id. The decorators hide the description
    // while an error is shown, so in practice at most one id is present; the
    // ordering rule is preserved for the case both are authored (WC light DOM).
    const describedby = [hasError ? ids.error : "", hasDescription ? ids.description : ""]
      .filter((id) => id !== "")
      .join(" ");
    return {
      control: {
        "aria-describedby": describedby === "" ? undefined : describedby,
        "aria-invalid": hasError ? "true" : undefined,
        "aria-required": config.required ? "true" : undefined,
      },
    };
  },
  // No key the field claims -- the slotted control owns every keystroke.
  keymap: () => null,
};

export const fieldBehavior: BehaviorSpec<FieldConfig, FieldState, FieldActions, FieldPart> =
  compose("field", field);

/** The description id derived from the field id (the sibling id scheme). */
export function descriptionId(fieldId: string): string {
  return `${fieldId}-description`;
}

/** The error id derived from the field id (the sibling id scheme). */
export function errorId(fieldId: string): string {
  return `${fieldId}-error`;
}

/**
 * Locate the field's control. Prefer the explicit part marker; fall back to the
 * first native form control so a light-DOM/slotted control the author did not
 * annotate is still found (and then stamped with `data-part="control"` so the
 * conformance harness and any re-query can find it too).
 */
function findControl(root: HTMLElement): HTMLElement | null {
  return (
    root.querySelector<HTMLElement>('[data-part="control"]') ??
    root.querySelector<HTMLElement>("input, select, textarea")
  );
}

/**
 * The DOM-native client of the field score -- shared by the Web Component and
 * the Astro `<script>`. React (retained mode) reads the projection
 * declaratively instead.
 *
 * Near-empty by construction: the score has no state and no actions, so there
 * is no createBehavior, no memory, no dispatch, and no keydown listener. bind
 * does exactly the wiring SSR could not reach into the slotted control:
 *  1. associate the label with the control (native `for`/`id`);
 *  2. apply the score's ARIA projection to the control;
 *  3. propagate the field-level `disabled` to the control (native).
 *
 * Bind-once: like input and radio-group, the client wires the server/author
 * markup a single time. Runtime attribute re-toggling (error added, disabled
 * flipped) is React's declarative affordance, not a WC lifecycle the client
 * re-observes -- a documented disposition in field.md.
 */
export function bindField(root: HTMLElement): () => void {
  const control = findControl(root);
  if (!control) return () => {};
  // Stamp the part marker so the harness (and a re-query) find the control even
  // when the author slotted a bare <input>.
  control.setAttribute("data-part", "control");

  const label = root.querySelector<HTMLElement>('[data-part="label"]');
  const description = root.querySelector<HTMLElement>('[data-part="description"]');
  const error = root.querySelector<HTMLElement>('[data-part="error"]');

  // Single deterministic sources: the field id IS the control's id (never
  // generated); disabled/required are host-level signals because they must
  // reach a slotted control SSR cannot touch.
  const disabled = root.hasAttribute("data-disabled");
  const config: FieldConfig = {
    required: root.hasAttribute("data-required"),
    disabled,
  };

  // ids READ from the rendered parts, never generated (Spec 01).
  const ids: PartIds<FieldPart> = {
    label: label?.id ?? "",
    control: control.id,
    description: description?.id ?? "",
    error: error?.id ?? "",
  };

  // Native label<->control association: point the label at whatever id the
  // control actually carries (the earned WC semantic -- author-supplied ids win).
  if (label && control.id !== "") label.setAttribute("for", control.id);

  // The projection is already resolved (final strings, undefined = absent), so
  // apply it raw: validate:false skips aria-manager's author-input coercion.
  const projection = fieldBehavior.aria({}, config, ids);
  const controlAria = projection.control;
  if (controlAria) {
    for (const [name, value] of Object.entries(controlAria)) {
      updateAriaAttribute(control, name as never, value as never, { validate: false });
    }
  }

  // disabled propagation -- native attribute AND property, so a disabled field
  // both reflects and actually disables the control.
  if (disabled) {
    control.setAttribute("disabled", "");
    if ("disabled" in control) (control as HTMLInputElement).disabled = true;
  }

  return () => {};
}

/** The resolved control ARIA for a given field id, for decorators that project
 *  declaratively (React) -- error/description ids collapse to '' when absent so
 *  the projection drops the reference rather than dangle it. */
export function fieldControlAria(
  fieldId: string,
  config: FieldConfig,
  hasError: boolean,
  hasDescription: boolean,
): AriaAttrs {
  const ids: PartIds<FieldPart> = {
    label: "",
    control: fieldId,
    description: hasDescription ? descriptionId(fieldId) : "",
    error: hasError ? errorId(fieldId) : "",
  };
  return fieldBehavior.aria({}, config, ids).control ?? {};
}
