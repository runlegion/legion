import { labelClasses } from "@/components/ui/label.classes";
import type { FieldConfig, FieldState } from "@/components/ui/field.behavior";

/**
 * Field decoration. The field is a layout-composition wrapper: the container
 * stacks label + control + helper/error with consistent spacing; the helper and
 * error share the small body-text role token and flip only their semantic
 * colour. Token/semantic classes only -- `text-body-small ts-body-small` is the typography
 * role token (never a raw `text-sm`), colours are the frozen semantic tokens.
 */
export interface FieldClassSet {
  container: string;
  /** The label class WITHOUT the disabled dim; use `labelClass(disabled)` for
   *  the resolved string the decorators paint. */
  label: string;
  requiredMarker: string;
  description: string;
  error: string;
}

const fieldContainerClasses = "flex flex-col gap-2";
const fieldLabelDisabledClasses = "opacity-50";
const fieldRequiredMarkerClasses = "text-destructive ml-1";
const fieldDescriptionClasses = "text-body-small ts-body-small text-muted-foreground";
const fieldErrorClasses = "text-body-small ts-body-small text-destructive";

/**
 * Compose the label's class string. Reuses the Label score's own decoration
 * (never a parallel hand-written map) plus the field's disabled dim, exactly as
 * the React performance composes via the `<Label>` component.
 */
export function composeFieldLabelClasses(disabled: boolean): string {
  const base = labelClasses({}, {}).root;
  return disabled ? `${base} ${fieldLabelDisabledClasses}` : base;
}

export function fieldClassSet(_config: FieldConfig, _state: FieldState): FieldClassSet {
  return {
    container: fieldContainerClasses,
    label: composeFieldLabelClasses(false),
    requiredMarker: fieldRequiredMarkerClasses,
    description: fieldDescriptionClasses,
    error: fieldErrorClasses,
  };
}
