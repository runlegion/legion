/**
 * Shared button variant and size class definitions
 *
 * Imported by both button.tsx (React) and button.astro (Astro)
 * to ensure visual parity across framework targets.
 */

export const buttonVariantClasses: Record<string, string> = {
  default:
    'bg-primary text-primary-foreground ' +
    'hover:bg-primary-hover active:bg-primary-active ' +
    'focus-visible:ring-2 focus-visible:ring-primary-ring',
  primary:
    'bg-primary text-primary-foreground ' +
    'hover:bg-primary-hover active:bg-primary-active ' +
    'focus-visible:ring-2 focus-visible:ring-primary-ring',
  secondary:
    'bg-secondary text-secondary-foreground ' +
    'hover:bg-secondary-hover active:bg-secondary-active ' +
    'focus-visible:ring-2 focus-visible:ring-secondary-ring',
  destructive:
    'bg-destructive text-destructive-foreground ' +
    'hover:bg-destructive-hover active:bg-destructive-active ' +
    'focus-visible:ring-2 focus-visible:ring-destructive-ring',
  success:
    'bg-success text-success-foreground ' +
    'hover:bg-success-hover active:bg-success-active ' +
    'focus-visible:ring-2 focus-visible:ring-success-ring',
  warning:
    'bg-warning text-warning-foreground ' +
    'hover:bg-warning-hover active:bg-warning-active ' +
    'focus-visible:ring-2 focus-visible:ring-warning-ring',
  info:
    'bg-info text-info-foreground ' +
    'hover:bg-info-hover active:bg-info-active ' +
    'focus-visible:ring-2 focus-visible:ring-info-ring',
  muted:
    'bg-muted text-muted-foreground ' +
    'hover:bg-muted-hover active:bg-muted-active ' +
    'focus-visible:ring-2 focus-visible:ring-ring',
  accent:
    'bg-accent text-accent-foreground ' +
    'hover:bg-accent-hover active:bg-accent-active ' +
    'focus-visible:ring-2 focus-visible:ring-accent-ring',
  outline:
    'border border-input bg-transparent text-foreground ' +
    'hover:bg-accent hover:text-accent-foreground ' +
    'focus-visible:ring-2 focus-visible:ring-ring',
  ghost:
    'bg-transparent text-foreground ' +
    'hover:bg-accent hover:text-accent-foreground ' +
    'focus-visible:ring-2 focus-visible:ring-ring',
  link:
    'text-primary underline-offset-4 ' +
    'hover:underline ' +
    'focus-visible:ring-2 focus-visible:ring-ring',
};

export const buttonSizeClasses: Record<string, string> = {
  default: 'h-10 px-4 py-2',
  xs: 'h-6 px-2 text-label-small',
  sm: 'h-8 px-3 text-label-small',
  lg: 'h-12 px-6 text-label-large',
  icon: 'h-10 w-10',
  'icon-xs': 'h-6 w-6',
  'icon-sm': 'h-8 w-8',
  'icon-lg': 'h-12 w-12',
};

export const buttonBaseClasses =
  'inline-flex items-center justify-center gap-2 rounded-md text-label-large cursor-pointer ' +
  'focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-offset-2 ' +
  'transition-colors duration-150 motion-reduce:transition-none ' +
  'disabled:opacity-50 disabled:cursor-not-allowed disabled:pointer-events-none';
