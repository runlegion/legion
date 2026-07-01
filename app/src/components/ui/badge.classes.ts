/**
 * Shared badge variant and size class definitions
 *
 * Imported by both badge.tsx (React) and badge.astro (Astro)
 * to ensure visual parity across framework targets.
 */

export const badgeVariantClasses: Record<string, string> = {
  default: 'bg-primary text-primary-foreground',
  primary: 'bg-primary text-primary-foreground',
  secondary: 'bg-secondary text-secondary-foreground',
  destructive: 'bg-destructive text-destructive-foreground',
  success: 'bg-success text-success-foreground',
  warning: 'bg-warning text-warning-foreground',
  info: 'bg-info text-info-foreground',
  muted: 'bg-muted text-muted-foreground',
  accent: 'bg-accent text-accent-foreground',
  outline: 'bg-transparent border border-input text-foreground',
  ghost: 'hover:bg-muted hover:text-muted-foreground',
  link: 'text-primary underline-offset-4 hover:underline',
};

export const badgeSizeClasses: Record<string, string> = {
  sm: 'px-2 py-0.5 text-label-small',
  default: 'px-2.5 py-0.5 text-label-small',
  lg: 'px-3 py-1 text-label-medium',
};

export const badgeBaseClasses =
  'inline-flex items-center justify-center rounded-full transition-colors duration-150 motion-reduce:transition-none';
