import type { AlertConfig, AlertState, AlertVariant } from "@/components/ui/alert.behavior";

export interface AlertClassSet {
  root: string;
}

const baseClasses =
  "relative w-full rounded-lg border p-4 " +
  "[&>svg]:absolute [&>svg]:left-4 [&>svg]:top-4 [&:has(svg)]:pl-11 [&>svg+div]:-translate-y-0.5";

/**
 * Subtle background paired with its OWN subtle-foreground token, never the
 * solid variant's foreground -- the oracle pairs `bg-*-subtle` with the
 * solid `text-*-foreground`, which is contrast-tuned against the SOLID
 * fill, not the subtle one (defect-do-not-port, see the component doc).
 * `muted` has no subtle tier in the registry, so it keeps the flat
 * muted/border pairing the oracle already used.
 */
const variantClasses: Record<AlertVariant, string> = {
  default: "bg-primary-subtle text-primary-subtle-foreground border-primary-border",
  primary: "bg-primary-subtle text-primary-subtle-foreground border-primary-border",
  secondary: "bg-secondary-subtle text-secondary-subtle-foreground border-secondary-border",
  destructive: "bg-destructive-subtle text-destructive-subtle-foreground border-destructive-border",
  success: "bg-success-subtle text-success-subtle-foreground border-success-border",
  warning: "bg-warning-subtle text-warning-subtle-foreground border-warning-border",
  info: "bg-info-subtle text-info-subtle-foreground border-info-border",
  muted: "bg-muted text-muted-foreground border-border",
  accent: "bg-accent-subtle text-accent-subtle-foreground border-accent-border",
};

/** Sub-parts are config-independent, so the framework file imports these
 *  literals directly (no context/provider needed for a flat static). */
export const alertTitleClasses = "mb-1 text-title-small ts-title-small leading-none";

export const alertDescriptionClasses = "text-body-small ts-body-small [&_p]:leading-relaxed";

export const alertActionClasses = "ml-auto shrink-0";

export function alertClasses(config: AlertConfig, _state: AlertState): AlertClassSet {
  const variant = config.variant ?? "default";
  // The class set carries only declared parts (Spec 01: Partial<Record<Part,
  // string>>); the sub-component literals above are the direct imports.
  return {
    root: `${baseClasses} ${variantClasses[variant]}`,
  };
}
