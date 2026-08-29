import type { LabelConfig, LabelState, LabelVariant } from "@/components/ui/label.behavior";

export interface LabelClassSet {
  root: string;
}

/**
 * Structure every label carries: the label typography role token, tight
 * leading, and the peer-disabled affordance (dim + not-allowed cursor when a
 * sibling `.peer` control is disabled). Ported verbatim from the oracle's
 * settled decoration -- `text-label-medium ts-label-medium` is the semantic typography role
 * token, never a raw font size.
 */
const labelBaseClasses =
  "text-label-medium ts-label-medium leading-none peer-disabled:cursor-not-allowed peer-disabled:opacity-70";

/**
 * Semantic colour variants (rafters extension). Each is a `text-{role}` token
 * over the frozen colour vocabulary -- semantic classes only, never a raw
 * colour utility. The variant is a colour channel; it does not change the
 * label's association or role.
 */
export const labelVariantClasses: Record<LabelVariant, string> = {
  default: "text-foreground",
  primary: "text-primary",
  secondary: "text-secondary",
  destructive: "text-destructive",
  success: "text-success",
  warning: "text-warning",
  info: "text-info",
  muted: "text-muted-foreground",
  accent: "text-accent",
};

function resolveVariant(variant: LabelConfig["variant"]): LabelVariant {
  return variant && variant in labelVariantClasses ? variant : "default";
}

export function labelClasses(config: LabelConfig, _state: LabelState): LabelClassSet {
  const variant = resolveVariant(config.variant);
  return { root: `${labelBaseClasses} ${labelVariantClasses[variant]}` };
}
