/**
 * HoverCard component for rich preview content on hover
 *
 * @cognitive-load 3/10 - Contextual preview that supplements rather than replaces visible content
 * @attention-economics Glanceable enrichment: provides additional context without requiring action
 * @trust-building Predictable reveal timing, stable positioning, non-disruptive appearance
 * @accessibility Focus management, keyboard triggerable via focus, escape to dismiss, role="dialog" with aria-describedby
 * @semantic-meaning Rich preview: profile cards, link previews, contextual details that enhance understanding
 *
 * @usage-patterns
 * DO: Show supplementary information like user profiles, link previews, or contextual details
 * DO: Trust the system hover-intent delay -- it is a token the stylesheet applies, not a per-instance tuning knob
 * DO: Keep content focused and scannable - users glance, not read
 * DO: Position intelligently to avoid viewport edges
 * NEVER: Essential information that should always be visible
 * NEVER: Interactive forms or multi-step workflows (use Popover instead)
 * NEVER: Time-sensitive content that disappears before user can read it
 *
 * @example
 * ```tsx
 * <HoverCard>
 *   <HoverCard.Trigger asChild>
 *     <a href="/user/john">@john</a>
 *   </HoverCard.Trigger>
 *   <HoverCard.Content>
 *     <div className="flex gap-4">
 *       <Avatar src="/john.jpg" />
 *       <div>
 *         <h4>John Doe</h4>
 *         <p>Software Engineer</p>
 *       </div>
 *     </div>
 *   </HoverCard.Content>
 * </HoverCard>
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
  hoverCard,
  isOpen,
  positionHoverCardContent,
  type HoverCardActions,
  type HoverCardConfig,
  type HoverCardPart,
  type HoverCardState,
} from '@/components/ui/hover-card.behavior';
import { hoverCardClasses, type HoverCardClassSet } from '@/components/ui/hover-card.classes';

// ==================== Root context ====================

interface HoverCardContextValue {
  state: HoverCardState;
  config: HoverCardConfig;
  effectiveOpen: boolean;
  ids: PartIds<HoverCardPart>;
  aria: Partial<Record<HoverCardPart, AriaAttrs>>;
  classes: HoverCardClassSet;
  request: (action: keyof HoverCardActions) => boolean;
  triggerRef: React.RefObject<HTMLElement | null>;
  disableHoverableContent: boolean;
  /** Escape sets it, a pointer leave or blur clears it (WCAG 1.4.13). */
  setDismissed: (dismissed: boolean) => void;
}

const HoverCardContext = React.createContext<HoverCardContextValue | null>(null);

function useHoverCardContext(component: string): HoverCardContextValue {
  const context = React.useContext(HoverCardContext);
  if (!context) {
    throw new Error(`${component} must be used within <HoverCard>`);
  }
  return context;
}

export interface HoverCardProps {
  children: React.ReactNode;
  open?: boolean;
  defaultOpen?: boolean;
  onOpenChange?: (open: boolean) => void;
  /** When true, the pointer cannot travel onto the content to hold it open. */
  disableHoverableContent?: boolean;
  side?: Side;
  align?: Align;
  sideOffset?: number;
}

/**
 * The root renders a REAL wrapper element (#2148), matching hover-card.astro's
 * `<div data-part="root" data-hover-card>`, so trigger and content are DOM
 * siblings under a hoverable root. That sibling shape is the whole CSS
 * contract: the stylesheet reveals the preview through `[data-hover-card]:hover
 * > [data-part=content]`, which a context-only root (rendering no node at all)
 * gave it nowhere to attach.
 */
export function HoverCardRoot({
  children,
  open,
  defaultOpen = false,
  onOpenChange,
  disableHoverableContent = false,
  side,
  align,
  sideOffset,
}: HoverCardProps) {
  const config: HoverCardConfig = {
    open,
    defaultOpen,
    disableHoverableContent,
    side,
    align,
    sideOffset,
  };

  const { memory, dispatch } = React.useMemo(() => createBehavior(hoverCard, config), []);
  const state = useMemory(memory);
  const effectiveOpen = isOpen(state, config);
  const [dismissed, setDismissed] = React.useState(false);

  const uid = React.useId();
  const ids = React.useMemo(() => {
    const out = {} as PartIds<HoverCardPart>;
    for (const part of Object.keys(hoverCard.parts) as HoverCardPart[])
      out[part] = `${uid}-${part}`;
    return out;
  }, [uid]);

  const triggerRef = React.useRef<HTMLElement | null>(null);

  const latest = React.useRef({ config, onOpenChange });
  latest.current = { config, onOpenChange };
  const request = React.useCallback(
    (action: keyof HoverCardActions): boolean => {
      const { config: cfg, onOpenChange: cb } = latest.current;
      if (!dispatch(action, cfg)) return false;
      cb?.(action === 'open');
      return true;
    },
    [dispatch],
  );

  const aria = hoverCard.aria(state, config, ids);

  const contextValue: HoverCardContextValue = {
    state,
    config,
    effectiveOpen,
    ids,
    aria,
    classes: hoverCardClasses(config, state),
    request,
    triggerRef,
    disableHoverableContent,
    setDismissed,
  };

  // The hover SCOPE mirrors the CSS reveal rule: the root by default (so the
  // pointer can travel onto the preview), the trigger alone when the content is
  // declared un-hoverable. These handlers move `data-state`, `onOpenChange`,
  // and positioning -- visibility belongs to the stylesheet, always, and there
  // is no timer on either side of the transition.
  //
  // The dismissal is dropped only once nothing can still reveal the card: the
  // trigger's `:focus-visible` is a reveal half of its own, so a pointerleave
  // that cleared the flag while the trigger still held focus would put the
  // dismissed card straight back up (WCAG 1.4.13). The trigger's own blur checks
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
    <HoverCardContext.Provider value={contextValue}>
      <div
        data-part="root"
        data-hover-card
        data-disable-hoverable-content={String(disableHoverableContent)}
        data-dismissed={dismissed ? 'true' : undefined}
        {...scopeHandlers}
      >
        {children}
      </div>
    </HoverCardContext.Provider>
  );
}

// ==================== Trigger ====================

export interface HoverCardTriggerProps extends React.AnchorHTMLAttributes<HTMLAnchorElement> {
  asChild?: boolean;
}

export function HoverCardTrigger({
  asChild,
  onPointerEnter,
  onPointerLeave,
  onFocus,
  onBlur,
  onKeyDown,
  children,
  ...props
}: HoverCardTriggerProps) {
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
  } = useHoverCardContext('HoverCardTrigger');

  const setRef = React.useCallback(
    (element: HTMLElement | null) => {
      triggerRef.current = element;
    },
    [triggerRef],
  );

  const handlePointerEnter = (event: React.PointerEvent<HTMLAnchorElement>) => {
    onPointerEnter?.(event);
    // Only when the root is NOT the hover scope -- otherwise the root already
    // covers the trigger and this would double-dispatch.
    if (disableHoverableContent) request('open');
  };
  const handlePointerLeave = (event: React.PointerEvent<HTMLAnchorElement>) => {
    onPointerLeave?.(event);
    if (disableHoverableContent) {
      // Same handoff as the root scope: the focus half of the reveal rule may
      // still be matching, and clearing under it re-reveals the dismissed card.
      if (!event.currentTarget.contains(document.activeElement)) setDismissed(false);
      request('close');
    }
  };
  const handleFocus = (event: React.FocusEvent<HTMLAnchorElement>) => {
    onFocus?.(event);
    request('open');
  };
  const handleBlur = (event: React.FocusEvent<HTMLAnchorElement>) => {
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
  const handleKeyDown = (event: React.KeyboardEvent<HTMLAnchorElement>) => {
    onKeyDown?.(event);
    if (event.defaultPrevented) return;
    // The score claims Escape. Dismissal is two moves: close the score (so
    // data-state and onOpenChange agree) and raise `data-dismissed` on the root,
    // which the stylesheet force-hides on even while `:hover` still matches.
    if (
      hoverCard.keymap(keyInputOf(event), { open: effectiveOpen }, 'trigger', config) === 'close'
    ) {
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
    <a
      ref={setRef as React.Ref<HTMLAnchorElement>}
      className={classy(classes.trigger, props.className)}
      {...partProps}
      {...props}
    >
      {children}
    </a>
  );
}

// ==================== Portal ====================

export interface HoverCardPortalProps {
  children: React.ReactNode;
  /** Portal target; defaults to document.body. */
  container?: HTMLElement | null;
  forceMount?: boolean;
}

/**
 * The explicit escape hatch for a clipping ancestor. Reaching for it OPTS THAT
 * INSTANCE OUT of the CSS sibling contract (#2148): a `document.body`-portaled
 * content node is not a DOM sibling of its trigger, so `[data-hover-card]:hover
 * > [data-part=content]` can never match it and the preview becomes JS-only,
 * revealed through `data-state` alone. Un-portaled is the default for exactly
 * that reason.
 */
export function HoverCardPortal({ children, container, forceMount }: HoverCardPortalProps) {
  const { effectiveOpen } = useHoverCardContext('HoverCardPortal');
  if (!(forceMount || effectiveOpen)) return null;
  if (typeof document === 'undefined') return null;
  return createPortal(children, container ?? document.body);
}

// ==================== Content ====================

export interface HoverCardContentProps extends React.HTMLAttributes<HTMLDivElement> {
  side?: Side;
  align?: Align;
  sideOffset?: number;
}

export function HoverCardContent({
  side,
  align,
  sideOffset,
  className,
  children,
  ...props
}: HoverCardContentProps) {
  const { config, effectiveOpen, ids, aria, classes, triggerRef } =
    useHoverCardContext('HoverCardContent');
  const contentRef = React.useRef<HTMLDivElement | null>(null);

  // Positioning is a DOM side-effect: wire it here, but the placement decision
  // lives in the shared positionHoverCardContent composer over collision-detector.
  const placementConfig: HoverCardConfig = {
    ...config,
    side: side ?? config.side,
    align: align ?? config.align,
    sideOffset: sideOffset ?? config.sideOffset,
  };
  React.useEffect(() => {
    const trigger = triggerRef.current;
    const content = contentRef.current;
    if (!effectiveOpen || !trigger || !content) return;
    const update = () => positionHoverCardContent(trigger, content, placementConfig);
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
  // the preview renders as a DOM sibling of its trigger and the stylesheet
  // decides whether it is visible. `hidden` is never applied for the same
  // reason -- it is UA `display: none`, which kills both the transition and the
  // reveal.
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

HoverCardRoot.displayName = 'HoverCard';
HoverCardTrigger.displayName = 'HoverCardTrigger';
HoverCardPortal.displayName = 'HoverCardPortal';
HoverCardContent.displayName = 'HoverCardContent';

/** shadcn drop-in surface: `HoverCard.Trigger` / `.Portal` / `.Content`, plus
 *  the named exports above for direct import. */
export const HoverCard = Object.assign(HoverCardRoot, {
  Trigger: HoverCardTrigger,
  Portal: HoverCardPortal,
  Content: HoverCardContent,
});
