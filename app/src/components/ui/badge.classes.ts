import type {
  BadgeConfig,
  BadgeSize,
  BadgeState,
  BadgeVariant,
} from "@/components/ui/badge.behavior";

export interface BadgeClassSet {
  root: string;
}

/**
 * Variant classes, ported verbatim from the oracle (src/old/ui/badge.classes.ts):
 * semantic token pairs (bg-X text-X-foreground) for the fill-family variants,
 * plus the shadcn structural variants (outline/ghost/link) that carry no fill.
 */
const variantClasses: Record<BadgeVariant, string> = {
  default: "bg-primary text-primary-foreground",
  primary: "bg-primary text-primary-foreground",
  secondary: "bg-secondary text-secondary-foreground",
  destructive: "bg-destructive text-destructive-foreground",
  success: "bg-success text-success-foreground",
  warning: "bg-warning text-warning-foreground",
  info: "bg-info text-info-foreground",
  muted: "bg-muted text-muted-foreground",
  accent: "bg-accent text-accent-foreground",
  outline: "bg-transparent border border-input text-foreground",
  ghost: "hover:bg-muted hover:text-muted-foreground",
  link: "text-primary underline-offset-4 hover:underline",
};

const sizeClasses: Record<BadgeSize, string> = {
  sm: "px-2 py-0.5 text-label-small ts-label-small",
  default: "px-2.5 py-0.5 text-label-small ts-label-small",
  lg: "px-3 py-1 text-label-medium ts-label-medium",
};

const baseClasses =
  "inline-flex items-center justify-center rounded-full transition-colors duration-150 motion-reduce:transition-none";

export function badgeClasses(config: BadgeConfig, _state: BadgeState): BadgeClassSet {
  const variant = config.variant ?? "default";
  const size = config.size ?? "default";
  return {
    root: `${baseClasses} ${variantClasses[variant]} ${sizeClasses[size]}`,
  };
}

/** The shadcn drop-in surface exports `badgeVariants` for styling non-span
 *  elements (`<a className={badgeVariants({...})}>`), the standard reach
 *  while `asChild` is deferred. Mirrors `buttonVariants`. */
export function badgeVariants(options: { variant?: BadgeVariant; size?: BadgeSize } = {}): string {
  return `${baseClasses} ${variantClasses[options.variant ?? "default"]} ${sizeClasses[options.size ?? "default"]}`;
}
