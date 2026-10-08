/**
 * Contextual tooltip component with smart timing and accessibility
 *
 * @cognitive-load 2/10 - Contextual help without interrupting user workflow
 * @attention-economics Non-intrusive assistance: Smart delays prevent accidental triggers while ensuring help availability
 * @trust-building Reliable contextual guidance that builds user confidence through progressive disclosure
 * @accessibility Keyboard navigation, screen reader support, focus management, escape key handling
 * @semantic-meaning Contextual assistance: help=functionality explanation, definition=terminology clarification, action=shortcuts and outcomes, status=system state
 *
 * @usage-patterns
 * DO: Explain functionality without overwhelming users
 * DO: Clarify terminology contextually when needed
 * DO: Show shortcuts and expected action outcomes
 * DO: Provide feedback on system state changes
 * NEVER: Include essential information that should be visible by default
 *
 * @example
 * ```tsx
 * <Tooltip.Provider>
 *   <Tooltip>
 *     <Tooltip.Trigger asChild>
 *       <Button>Hover me</Button>
 *     </Tooltip.Trigger>
 *     <Tooltip.Content>
 *       Helpful tooltip text
 *     </Tooltip.Content>
 *   </Tooltip>
 * </Tooltip.Provider>
 * ```
 */
import * as React from 'react';
import { createPortal } from 'react-dom';
import { keyInputOf } from '@/hooks/key-input';
import { useMemory } from '@/hooks/use-memory';
import classy from '@/lib/primitives/classy';
import { mergeProps } from '@/lib/primitives/slot';
import type { Align, Side } from '@/lib/primitives/types';
import { createBehavior, type AriaAttrs, type PartIds } from '@/lib/contract';
import {
  isOpen,
  positionTooltipContent,
  tooltip,
  type TooltipActions,
  type TooltipConfig,
  type TooltipPart,
  type TooltipState,
} from '@/components/ui/tooltip.behavior';
import { tooltipClasses, type TooltipClassSet } from '@/components/ui/tooltip.classes';

// ==================== Provider (shared defaults) ====================

/**
 * The delay props are GONE (#2148). Hover-intent timing is `transition-delay`
 * on `--rafters-delay-hover-intent` in tooltip.classes.ts -- a system decision,
 * not a per-instance or per-provider tuning knob. What is left here is the one
 * genuinely per-tree default: whether the pointer may travel onto the tip.
 */
interface TooltipProviderContextValue {
  disableHoverableContent: boolean;
}

const TooltipProviderContext = React.createContext<TooltipProviderContextValue>({
  disableHoverableContent: false,
});

export interface TooltipProviderProps {
  disableHoverableContent?: boolean;
  children: React.ReactNode;
}

export function TooltipProvider({
  disableHoverableContent = false,
  children,
}: TooltipProviderProps) {
  const value = React.useMemo(() => ({ disableHoverableContent }), [disableHoverableContent]);
  return (
    <TooltipProviderContext.Provider value={value}>{children}</TooltipProviderContext.Provider>
  );
}

// ==================== Root context ====================

interface TooltipContextValue {
  state: TooltipState;
  config: TooltipConfig;
  effectiveOpen: boolean;
  ids: PartIds<TooltipPart>;
  aria: Partial<Record<TooltipPart, AriaAttrs>>;
  classes: TooltipClassSet;
  request: (action: keyof TooltipActions) => boolean;
  triggerRef: React.RefObject<HTMLElement | null>;
  disableHoverableContent: boolean;
  /** Escape sets it, a pointer leave or blur clears it (WCAG 1.4.13). */
  setDismissed: (dismissed: boolean) => void;
}

const TooltipContext = React.createContext<TooltipContextValue | null>(null);

function useTooltipContext(component: string): TooltipContextValue {
  const context = React.useContext(TooltipContext);
  if (!context) {
    throw new Error(`${component} must be used within <Tooltip>`);
  }
  return context;
}

export interface TooltipProps {
  children: React.ReactNode;
  open?: boolean;
  defaultOpen?: boolean;
  onOpenChange?: (open: boolean) => void;
  side?: Side;
  align?: Align;
  sideOffset?: number;
}

/**
 * The root renders a REAL wrapper element (#2148), matching tooltip.astro's
 * `<div data-part="root" data-tooltip>`, so trigger and content are DOM
 * siblings under a hoverable root. That sibling shape is the whole CSS
 * contract: the stylesheet reveals the tip through `[data-tooltip]:hover >
 * [data-part=content]`, which a context-only root (rendering no node at all)
 * gave it nowhere to attach.
 */
export function TooltipRoot({
  children,
  open,
  defaultOpen = false,
  onOpenChange,
  side,
  align,
  sideOffset,
}: TooltipProps) {
  const provider = React.useContext(TooltipProviderContext);
  const config: TooltipConfig = {
    open,
    defaultOpen,
    disableHoverableContent: provider.disableHoverableContent,
    side,
    align,
    sideOffset,
  };

  const { memory, dispatch } = React.useMemo(() => createBehavior(tooltip, config), []);
  const state = useMemory(memory);
  const effectiveOpen = isOpen(state, config);
  const [dismissed, setDismissed] = React.useState(false);

  const uid = React.useId();
  const ids = React.useMemo(() => {
    const out = {} as PartIds<TooltipPart>;
    for (const part of Object.keys(tooltip.parts) as TooltipPart[]) out[part] = `${uid}-${part}`;
    return out;
  }, [uid]);

  const triggerRef = React.useRef<HTMLElement | null>(null);

  const latest = React.useRef({ config, onOpenChange });
  latest.current = { config, onOpenChange };
  const request = React.useCallback(
    (action: keyof TooltipActions): boolean => {
      const { config: cfg, onOpenChange: cb } = latest.current;
      if (!dispatch(action, cfg)) return false;
      cb?.(action === 'open');
      return true;
    },
    [dispatch],
  );

  const aria = tooltip.aria(state, config, ids);
  const disableHoverableContent = config.disableHoverableContent ?? false;

  const contextValue: TooltipContextValue = {
    state,
    config,
    effectiveOpen,
    ids,
    aria,
    classes: tooltipClasses(config, state),
    request,
    triggerRef,
    disableHoverableContent,
    setDismissed,
  };

  // The hover SCOPE mirrors the CSS reveal rule: the root by default (so the
  // pointer can travel onto the tip), the trigger alone when the content is
  // declared un-hoverable. These handlers move `data-state`, `onOpenChange`,
  // and positioning -- visibility belongs to the stylesheet, always, and there
  // is no timer on either side of the transition.
  //
  // The dismissal is dropped only once nothing can still reveal the tip: the
  // trigger's `:focus-visible` is a reveal half of its own, so a pointerleave
  // that cleared the flag while the trigger still held focus would put the
  // dismissed tip straight back up (WCAG 1.4.13). The trigger's own blur checks
  // the hover axis in the same way -- whichever leaves last does the clear.
  const scopeHandlers = disableHoverableContent
    ? {}
    : {
        onPointerEnter: () => request('open'),
        onPointerLeave: (event: React.PointerEvent<HTMLDivElement>) => {
          if (!event.currentTarget.contains(document.activeElement)) setDismissed(false);
          request('close');
        },
      };

  return (
    <TooltipContext.Provider value={contextValue}>
      <div
        data-part="root"
        data-tooltip
        data-disable-hoverable-content={String(disableHoverableContent)}
        data-dismissed={dismissed ? 'true' : undefined}
        {...scopeHandlers}
      >
        {children}
      </div>
    </TooltipContext.Provider>
  );
}

// ==================== Trigger ====================

export interface TooltipTriggerProps extends React.ButtonHTMLAttributes<HTMLButtonElement> {
  asChild?: boolean;
}

export function TooltipTrigger({
  asChild,
  onPointerEnter,
  onPointerLeave,
  onFocus,
  onBlur,
  onKeyDown,
  children,
  ...props
}: TooltipTriggerProps) {
  const {
    config,
    effectiveOpen,
    ids,
    aria,
    classes,
    triggerRef,
    request,
    disableHoverableContent,
    setDismissed,
  } = useTooltipContext('TooltipTrigger');

  const setRef = React.useCallback(
    (element: HTMLElement | null) => {
      triggerRef.current = element;
    },
    [triggerRef],
  );

  const handlePointerEnter = (event: React.PointerEvent<HTMLButtonElement>) => {
    onPointerEnter?.(event);
    // Only when the root is NOT the hover scope -- otherwise the root already
    // covers the trigger and this would double-dispatch.
    if (disableHoverableContent) request('open');
  };
  const handlePointerLeave = (event: React.PointerEvent<HTMLButtonElement>) => {
    onPointerLeave?.(event);
    if (disableHoverableContent) {
      // Same handoff as the root scope: the focus half of the reveal rule may
      // still be matching, and clearing under it re-reveals the dismissed tip.
      if (!event.currentTarget.contains(document.activeElement)) setDismissed(false);
      request('close');
    }
  };
  const handleFocus = (event: React.FocusEvent<HTMLButtonElement>) => {
    onFocus?.(event);
    request('open');
  };
  const handleBlur = (event: React.FocusEvent<HTMLButtonElement>) => {
    onBlur?.(event);
    // The other half of the handoff: the hover scope -- the root, or the trigger
    // alone when the content is un-hoverable -- may still be `:hover`, and the
    // reveal rule does not care that focus has gone.
    const scope = disableHoverableContent
      ? event.currentTarget
      : event.currentTarget.closest('[data-part="root"]');
    if (scope?.matches(':hover') !== true) setDismissed(false);
    request('close');
  };
  const handleKeyDown = (event: React.KeyboardEvent<HTMLButtonElement>) => {
    onKeyDown?.(event);
    if (event.defaultPrevented) return;
    // The score claims Escape. Dismissal is two moves: close the score (so
    // data-state and onOpenChange agree) and raise `data-dismissed` on the root,
    // which the stylesheet force-hides on even while `:hover` still matches.
    if (tooltip.keymap(keyInputOf(event), { open: effectiveOpen }, 'trigger', config) === 'close') {
      event.preventDefault();
      request('close');
      setDismissed(true);
    }
  };

  const partProps = {
    'data-part': 'trigger',
    id: ids.trigger,
    ...aria.trigger,
    onPointerEnter: handlePointerEnter,
    onPointerLeave: handlePointerLeave,
    onFocus: handleFocus,
    onBlur: handleBlur,
    onKeyDown: handleKeyDown,
  };

  if (asChild && React.isValidElement(children)) {
    const childProps = children.props as Record<string, unknown>;
    return React.cloneElement(
      children,
      mergeProps({ ...partProps, ref: setRef }, childProps) as React.Attributes,
    );
  }

  return (
    <button
      type="button"
      ref={setRef as React.Ref<HTMLButtonElement>}
      className={classy(classes.trigger, props.className)}
      {...partProps}
      {...props}
    >
      {children}
    </button>
  );
}

// ==================== Portal ====================

export interface TooltipPortalProps {
  children: React.ReactNode;
  /** Portal target; defaults to document.body. */
  container?: HTMLElement | null;
  forceMount?: boolean;
}

/**
 * The explicit escape hatch for a clipping ancestor. Reaching for it OPTS THAT
 * INSTANCE OUT of the CSS sibling contract (#2148): a `document.body`-portaled
 * content node is not a DOM sibling of its trigger, so `[data-tooltip]:hover >
 * [data-part=content]` can never match it and the tip becomes JS-only, revealed
 * through `data-state` alone. Un-portaled is the default for exactly that
 * reason.
 */
export function TooltipPortal({ children, container, forceMount }: TooltipPortalProps) {
  const { effectiveOpen } = useTooltipContext('TooltipPortal');
  if (!(forceMount || effectiveOpen)) return null;
  if (typeof document === 'undefined') return null;
  return createPortal(children, container ?? document.body);
}

// ==================== Content ====================

export interface TooltipContentProps extends React.HTMLAttributes<HTMLDivElement> {
  side?: Side;
  align?: Align;
  sideOffset?: number;
}

export function TooltipContent({
  side,
  align,
  sideOffset,
  className,
  children,
  ...props
}: TooltipContentProps) {
  const { config, effectiveOpen, ids, aria, classes, triggerRef } =
    useTooltipContext('TooltipContent');
  const contentRef = React.useRef<HTMLDivElement | null>(null);

  // Positioning is a DOM side-effect: wire it here, but the placement decision
  // lives in the shared positionTooltipContent composer over collision-detector.
  const placementConfig: TooltipConfig = {
    ...config,
    side: side ?? config.side,
    align: align ?? config.align,
    sideOffset: sideOffset ?? config.sideOffset,
  };
  React.useEffect(() => {
    const trigger = triggerRef.current;
    const content = contentRef.current;
    if (!effectiveOpen || !trigger || !content) return;
    const update = () => positionTooltipContent(trigger, content, placementConfig);
    update();
    window.addEventListener('scroll', update, { capture: true, passive: true });
    window.addEventListener('resize', update, { passive: true });
    return () => {
      window.removeEventListener('scroll', update, { capture: true });
      window.removeEventListener('resize', update);
    };
  });

  // UNCONDITIONAL (#2148). A node that does not exist cannot be revealed by
  // `:hover`, so there is no `forceMount || open` gate and no automatic portal:
  // the tip renders as a DOM sibling of its trigger and the stylesheet decides
  // whether it is visible. `hidden` is never applied for the same reason -- it
  // is UA `display: none`, which kills both the transition and the reveal.
  const dataState = effectiveOpen ? 'open' : 'closed';
  return (
    <div
      data-part="content"
      id={ids.content}
      ref={contentRef}
      data-state={dataState}
      className={classy(classes.content, className)}
      {...aria.content}
      {...props}
    >
      {children}
    </div>
  );
}

// ==================== Display names + namespaced (shadcn) surface ====================

TooltipProvider.displayName = 'TooltipProvider';
TooltipRoot.displayName = 'Tooltip';
TooltipTrigger.displayName = 'TooltipTrigger';
TooltipPortal.displayName = 'TooltipPortal';
TooltipContent.displayName = 'TooltipContent';

/** shadcn drop-in surface: `Tooltip.Provider` / `.Trigger` / `.Portal` /
 *  `.Content`, plus the named exports above for direct import. */
export const Tooltip = Object.assign(TooltipRoot, {
  Provider: TooltipProvider,
  Trigger: TooltipTrigger,
  Portal: TooltipPortal,
  Content: TooltipContent,
});
