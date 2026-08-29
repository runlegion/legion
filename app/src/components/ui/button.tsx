/**
 * Interactive button component for user actions
 *
 * @cognitive-load 3/10 - Simple action trigger with clear visual hierarchy
 * @attention-economics Size hierarchy: sm=tertiary actions, default=secondary interactions, lg=primary calls-to-action. Primary variant commands highest attention - use sparingly (maximum 1 per section)
 * @trust-building Destructive actions require confirmation patterns. Loading states prevent double-submission. Visual feedback reinforces user actions.
 * @accessibility WCAG AAA compliant with 44px minimum touch targets, high contrast ratios, and screen reader optimization
 * @semantic-meaning Variant mapping: default=main actions, secondary=supporting actions, destructive=irreversible actions with safety patterns
 *
 * @usage-patterns
 * DO: Primary: Main user goal, maximum 1 per section
 * DO: Secondary: Alternative paths, supporting actions
 * DO: Destructive: Permanent actions, requires confirmation patterns
 * NEVER: Multiple primary buttons competing for attention
 *
 * @example
 * ```tsx
 * // Primary action - highest attention, use once per section
 * <Button variant="default">Save Changes</Button>
 *
 * // Destructive action - requires confirmation UX
 * <Button variant="destructive">Delete Account</Button>
 *
 * // Loading state - prevents double submission
 * <Button loading>Processing...</Button>
 * ```
 */
import * as React from "react";
import { createBehavior, type PartIds } from "@/lib/contract";
import { useMemory } from "@/hooks/use-memory";
import classy from "@/lib/primitives/classy";
import { announceToScreenReader } from "@/lib/primitives/sr-announcer";
import {
  button,
  type ButtonActions,
  type ButtonConfig,
  type ButtonPart,
  type ButtonSize,
  type ButtonVariant,
} from "@/components/ui/button.behavior";
import { buttonClasses } from "@/components/ui/button.classes";

export { buttonVariants } from "@/components/ui/button.classes";
export type { ButtonSize, ButtonVariant };

type NonIconSize = "default" | "xs" | "sm" | "lg";
type IconSize = "icon" | "icon-xs" | "icon-sm" | "icon-lg";
type AccessibleName = { "aria-label": string } | { "aria-labelledby": string };

/**
 * Action trigger. Dispatches a press; Enter/Space activate natively; loading
 * announces busy and gates re-activation.
 *
 * @cognitive-load 3/10 - decision 1, information 1, interaction 1, disruption
 * 0, learning 0. One control, one decision (activate or not); the loading and
 * pressed states add a little information to read. Universally learned
 * affordance, no disruption.
 * @attention-economics Primary-action surface: at most one high-emphasis
 * variant (default/primary/destructive) per view; ghost/link/outline are the
 * unlimited low-attention register.
 * @trust-building The double-submit guard and soft-disabled gate keep an
 * in-flight or unavailable action from firing twice or silently; the control
 * stays focusable and discoverable rather than vanishing behind
 * pointer-events.
 * @accessibility Native `<button>` semantics (role, Enter/Space) are
 * preserved. Loading keeps the label as the accessible name and adds an
 * aria-hidden spinner plus `aria-busy`; soft-disabled projects `aria-disabled`
 * while staying focusable; hard-disabled uses native `disabled` only. Toggle
 * mode projects `aria-pressed`. Icon-only sizes require an accessible name at
 * the type level.
 */
interface ButtonBaseProps extends React.ButtonHTMLAttributes<HTMLButtonElement> {
  variant?: ButtonVariant;
  loading?: boolean;
  softDisabled?: boolean;
  toggle?: boolean;
  pressed?: boolean;
  defaultPressed?: boolean;
  onPressedChange?: (pressed: boolean) => void;
  loadingAnnouncement?: string;
  loadedAnnouncement?: string;
}

export type ButtonProps = ButtonBaseProps &
  ({ size?: NonIconSize } | ({ size: IconSize } & AccessibleName));

export const Button = React.forwardRef<HTMLButtonElement, ButtonProps>((props, ref) => {
  const {
    className,
    variant = "default",
    size = "default",
    disabled = false,
    loading = false,
    softDisabled = false,
    toggle = false,
    pressed,
    defaultPressed = false,
    onPressedChange,
    loadingAnnouncement,
    loadedAnnouncement,
    onClick,
    children,
    type,
    ...rest
  } = props;

  const config: ButtonConfig = {
    variant,
    size,
    toggle,
    defaultPressed,
    loadingAnnouncement,
    loadedAnnouncement,
    disabled,
    softDisabled,
    loading,
  };

  // The controller composes the score with the substrate directly -- no
  // useBehavior. createBehavior is the model, useMemory subscribes, and the
  // effect below composes the sr-announcer primitive directly on the loading
  // false->true edge (a mount-already-loading button stays baseline-suppressed).
  const { memory, dispatch } = React.useMemo(() => createBehavior(button, config), []);
  const state = useMemory(memory);

  // Gotcha #1: the controlled callback compares the EFFECTIVE value before
  // (the `pressed` prop when controlled) against the INTRINSIC value after the
  // reducer -- never effective-vs-effective, which a controlled prop would
  // pin flat. A toggle press always flips, so no equality guard is needed;
  // canDispatch already gates the disabled/loading/soft-disabled cases.
  const latest = React.useRef({ config, toggle, pressed, onPressedChange });
  latest.current = { config, toggle, pressed, onPressedChange };
  const request = React.useCallback(
    (action: keyof ButtonActions): boolean => {
      const { config: cfg, toggle: tgl, pressed: ctrl, onPressedChange: cb } = latest.current;
      if (!dispatch(action, cfg)) return false;
      if (tgl) cb?.(ctrl === undefined ? memory.get().pressed === true : !ctrl);
      return true;
    },
    [dispatch, memory],
  );

  // Compose the sr-announcer primitive directly, edge-triggered: announce the
  // loading message once on the loading false->true transition, never on a
  // baseline mount. prevLoading seeds to the current loading so the first commit
  // is baseline; announceToScreenReader is called only inside the edge branch so
  // no live region is constructed for a button that never transitions.
  const prevLoading = React.useRef(loading);
  React.useEffect(() => {
    const wasLoading = prevLoading.current;
    prevLoading.current = loading;
    if (loading && !wasLoading) {
      announceToScreenReader(loadingAnnouncement ?? "Loading", "polite");
    }
  }, [loading, loadingAnnouncement]);

  const uid = React.useId();
  const ids = {} as PartIds<ButtonPart>;
  for (const part of Object.keys(button.parts) as ButtonPart[]) ids[part] = `${uid}-${part}`;
  const aria = button.aria(state, config, ids);
  const classes = buttonClasses(config, state);

  return (
    <button
      ref={ref}
      type={type ?? "button"}
      disabled={disabled}
      data-part="root"
      id={ids.root}
      className={classy(classes.root, className)}
      {...aria.root}
      onClick={(event) => {
        if (!request("press")) {
          event.preventDefault();
          event.stopPropagation();
          return;
        }
        onClick?.(event);
      }}
      {...rest}
    >
      {loading ? (
        <svg
          data-part="spinner"
          id={ids.spinner}
          className={classes.spinner}
          viewBox="0 0 24 24"
          fill="none"
          stroke="currentColor"
          strokeWidth="2"
          strokeLinecap="round"
          {...aria.spinner}
        >
          <path d="M21 12a9 9 0 1 1-6.219-8.56" />
        </svg>
      ) : null}
      <span data-part="label" id={ids.label}>
        {children}
      </span>
    </button>
  );
});

Button.displayName = "Button";
export default Button;
