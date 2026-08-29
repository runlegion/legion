import type {
  ButtonConfig,
  ButtonSize,
  ButtonState,
  ButtonVariant,
} from "@/components/ui/button.behavior";

export interface ButtonClassSet {
  root: string;
  spinner: string;
}

const variantClasses: Record<ButtonVariant, string> = {
  default:
    "bg-primary text-primary-foreground " +
    "hover:bg-primary-hover active:bg-primary-active " +
    "focus-visible:ring-2 focus-visible:ring-primary-ring",
  primary:
    "bg-primary text-primary-foreground " +
    "hover:bg-primary-hover active:bg-primary-active " +
    "focus-visible:ring-2 focus-visible:ring-primary-ring",
  secondary:
    "bg-secondary text-secondary-foreground " +
    "hover:bg-secondary-hover active:bg-secondary-active " +
    "focus-visible:ring-2 focus-visible:ring-secondary-ring",
  destructive:
    "bg-destructive text-destructive-foreground " +
    "hover:bg-destructive-hover active:bg-destructive-active " +
    "focus-visible:ring-2 focus-visible:ring-destructive-ring",
  success:
    "bg-success text-success-foreground " +
    "hover:bg-success-hover active:bg-success-active " +
    "focus-visible:ring-2 focus-visible:ring-success-ring",
  warning:
    "bg-warning text-warning-foreground " +
    "hover:bg-warning-hover active:bg-warning-active " +
    "focus-visible:ring-2 focus-visible:ring-warning-ring",
  info:
    "bg-info text-info-foreground " +
    "hover:bg-info-hover active:bg-info-active " +
    "focus-visible:ring-2 focus-visible:ring-info-ring",
  muted:
    "bg-muted text-muted-foreground " +
    "hover:bg-muted-hover active:bg-muted-active " +
    "focus-visible:ring-2 focus-visible:ring-ring",
  accent:
    "bg-accent text-accent-foreground " +
    "hover:bg-accent-hover active:bg-accent-active " +
    "focus-visible:ring-2 focus-visible:ring-accent-ring",
  outline:
    "border border-input bg-transparent text-foreground " +
    "hover:bg-accent hover:text-accent-foreground " +
    "focus-visible:ring-2 focus-visible:ring-ring",
  ghost:
    "bg-transparent text-foreground " +
    "hover:bg-accent hover:text-accent-foreground " +
    "focus-visible:ring-2 focus-visible:ring-ring",
  link:
    "text-primary underline-offset-4 " +
    "hover:underline " +
    "focus-visible:ring-2 focus-visible:ring-ring",
};

const sizeClasses: Record<ButtonSize, string> = {
  default: "h-11 px-4 py-2.5 @md:h-10 @md:py-2",
  xs: "h-11 px-3 text-label-small ts-label-small @md:h-6 @md:px-2",
  sm: "h-11 px-4 text-label-small ts-label-small @md:h-8 @md:px-3",
  lg: "h-12 px-6 text-label-large ts-label-large",
  icon: "h-11 w-11 @md:h-10 @md:w-10",
  "icon-xs": "h-11 w-11 @md:h-6 @md:w-6",
  "icon-sm": "h-11 w-11 @md:h-8 @md:w-8",
  "icon-lg": "h-12 w-12",
};

const baseClasses =
  "inline-flex items-center justify-center gap-2 rounded-md text-label-large ts-label-large cursor-pointer " +
  "focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-offset-2 " +
  "transition-colors duration-150 motion-reduce:transition-none " +
  "disabled:opacity-50 disabled:cursor-not-allowed " +
  "aria-disabled:opacity-50 aria-disabled:cursor-not-allowed " +
  "aria-busy:cursor-progress";

const spinnerClasses = "h-5 w-5 @md:h-4 @md:w-4 animate-spin motion-reduce:animate-none";

export function buttonClasses(config: ButtonConfig, _state: ButtonState): ButtonClassSet {
  return {
    root: `${baseClasses} ${variantClasses[config.variant]} ${sizeClasses[config.size]}`,
    spinner: spinnerClasses,
  };
}

export function buttonVariants(
  options: { variant?: ButtonVariant; size?: ButtonSize } = {},
): string {
  return `${baseClasses} ${variantClasses[options.variant ?? "default"]} ${sizeClasses[options.size ?? "default"]}`;
}
