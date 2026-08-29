/**
 * Sheet component for slide-in side panel overlays
 *
 * @cognitive-load 5/10 - Partial page overlay requiring focused attention
 * @attention-economics Partial attention capture: main content dimmed but visible, slide animation indicates temporary state
 * @trust-building Clear slide direction, easy dismissal via overlay click or escape, preserves main content context
 * @accessibility Focus trap within sheet, escape key closes, proper ARIA dialog role
 * @semantic-meaning Supplementary content: navigation, filters, forms that don't warrant full page navigation
 *
 * @usage-patterns
 * DO: Use for mobile navigation, filters, or secondary forms
 * DO: Choose side based on content relationship (left=nav, right=details)
 * DO: Provide clear close mechanism
 * DO: Keep content scoped to single purpose
 * NEVER: Primary content, complex multi-step workflows, content requiring full attention
 *
 * @example
 * ```tsx
 * // Minimal usage - Portal, Overlay, and Close button are included automatically
 * <Sheet>
 *   <SheetTrigger>Open</SheetTrigger>
 *   <SheetContent side="right">
 *     <SheetHeader>
 *       <SheetTitle>Title</SheetTitle>
 *       <SheetDescription>Description</SheetDescription>
 *     </SheetHeader>
 *     Content here
 *   </SheetContent>
 * </Sheet>
 *
 * // Or with namespace syntax
 * <Sheet>
 *   <Sheet.Trigger asChild>
 *     <Button variant="outline">Open</Button>
 *   </Sheet.Trigger>
 *   <Sheet.Content side="right">
 *     <Sheet.Header>
 *       <Sheet.Title>Sheet Title</Sheet.Title>
 *     </Sheet.Header>
 *     Sheet content here
 *   </Sheet.Content>
 * </Sheet>
 *
 * // Hide close button if needed
 * <SheetContent showCloseButton={false}>...</SheetContent>
 * ```
 */
import * as React from "react";
import { createPortal } from "react-dom";
import { createBehavior, type AriaAttrs, type PartIds } from "@/lib/contract";
import { keyInputOf } from "@/hooks/key-input";
import { useMemory } from "@/hooks/use-memory";
import { usePresence } from "@/hooks/use-presence";
import classy from "@/lib/primitives/classy";
import { mergeProps } from "@/lib/primitives/slot";
import {
  isOpen,
  sheet,
  startSheetModalEffects,
  type SheetActions,
  type SheetConfig,
  type SheetPart,
  type SheetSide,
  type SheetState,
} from "@/components/ui/sheet.behavior";
import {
  sheetClasses,
  sheetContentClasses,
  type SheetClassSet,
} from "@/components/ui/sheet.classes";

/** The oracle-compatible dismissal veto surface on SheetContent. */
interface DismissVetoCallbacks {
  onPointerDownOutside?: ((event: Event) => void) | undefined;
  onInteractOutside?: ((event: Event) => void) | undefined;
}

interface SheetContextValue {
  state: SheetState;
  ids: PartIds<SheetPart>;
  aria: Partial<Record<SheetPart, AriaAttrs>>;
  request: (action: keyof SheetActions) => boolean;
  setPart: (part: SheetPart) => (element: HTMLElement | null) => void;
  getPart: (part: string) => HTMLElement | null;
  config: SheetConfig;
  effectiveOpen: boolean;
  classes: SheetClassSet;
  dismissVetoRef: React.RefObject<DismissVetoCallbacks | null>;
}

const SheetContext = React.createContext<SheetContextValue | null>(null);

function useSheetContext(component: string): SheetContextValue {
  const context = React.useContext(SheetContext);
  if (!context) {
    throw new Error(`${component} must be used within <Sheet>`);
  }
  return context;
}

/** True when rendering inside an explicit <SheetPortal> (Radix-style
 *  composition); SheetContent then skips its automatic portal + overlay. */
const SheetPortalContext = React.createContext(false);

export interface SheetProps {
  children: React.ReactNode;
  open?: boolean;
  defaultOpen?: boolean;
  onOpenChange?: (open: boolean) => void;
  modal?: boolean;
}

export function Sheet({
  children,
  open,
  defaultOpen = false,
  onOpenChange,
  modal = true,
}: SheetProps) {
  const config: SheetConfig = { open, defaultOpen, modal };
  const dismissVetoRef = React.useRef<DismissVetoCallbacks | null>(null);

  // The controller composes the score with the substrate -- no useBehavior.
  const { memory, dispatch } = React.useMemo(() => createBehavior(sheet, config), []);
  const state = useMemory(memory);
  const effectiveOpen = isOpen(state, config);

  const uid = React.useId();

  // Content portals to document.body with a unique id, so getPart resolves by
  // id -- no ref registry. Optional parts still need a mount signal (setPart)
  // purely so an omitted description projects no dangling aria-describedby.
  const [presentParts, setPresentParts] = React.useState<ReadonlySet<string>>(new Set());
  const partCallbacks = React.useRef<Map<string, (el: HTMLElement | null) => void>>(new Map());
  const setPart = React.useCallback((part: SheetPart) => {
    let callback = partCallbacks.current.get(part);
    if (!callback) {
      callback = (element: HTMLElement | null) =>
        setPresentParts((previous) => {
          const present = element !== null;
          if (previous.has(part) === present) return previous;
          const next = new Set(previous);
          if (present) next.add(part);
          else next.delete(part);
          return next;
        });
      partCallbacks.current.set(part, callback);
    }
    return callback;
  }, []);
  const getPart = React.useCallback(
    (part: string): HTMLElement | null =>
      typeof document === "undefined" ? null : document.getElementById(`${uid}-${part}`),
    [uid],
  );

  // title and description are the UNGUARDED cross-ref sources (labelledby/
  // describedby have no `open` guard, unlike aria-controls), so an absent one
  // must resolve to an empty id. Every other part keeps a stable id -- content
  // especially, since the focus-trap effect finds it by id.
  const ids = React.useMemo(() => {
    const out = {} as PartIds<SheetPart>;
    for (const part of Object.keys(sheet.parts) as SheetPart[]) {
      const crossRefSource = part === "title" || part === "description";
      out[part] = crossRefSource && !presentParts.has(part) ? "" : `${uid}-${part}`;
    }
    return out;
  }, [uid, presentParts]);

  const latest = React.useRef({ config, onOpenChange });
  latest.current = { config, onOpenChange };
  const request = React.useCallback(
    (action: keyof SheetActions): boolean => {
      const { config: cfg, onOpenChange: cb } = latest.current;
      if (!dispatch(action, cfg)) return false;
      cb?.(action === "open");
      return true;
    },
    [dispatch],
  );

  // The modal overlay trio, composed directly on the open+modal transition
  // (the retired effects runner's replacement). Level-triggered via the
  // dependency array; the cleanup tears the trio down (focus restore rides the
  // trap teardown).
  React.useEffect(() => {
    if (!effectiveOpen || !modal) return;
    const content = getPart("content");
    if (!content) return;
    return startSheetModalEffects({
      content,
      getTrigger: () => getPart("trigger"),
      // Outside-pointerdown dismissals offer the consumer veto first (oracle
      // protocol: callbacks run, close proceeds unless defaultPrevented).
      onDismiss: (event) => {
        const veto = dismissVetoRef.current;
        if (veto) {
          veto.onPointerDownOutside?.(event);
          veto.onInteractOutside?.(event);
          if (event.defaultPrevented) return;
        }
        request("close");
      },
    });
  }, [effectiveOpen, modal, getPart, request]);

  const aria = sheet.aria(state, config, ids);

  const contextValue: SheetContextValue = {
    state,
    ids,
    aria,
    request,
    setPart,
    getPart,
    config,
    effectiveOpen,
    classes: sheetClasses(config, state),
    dismissVetoRef,
  };

  return <SheetContext.Provider value={contextValue}>{children}</SheetContext.Provider>;
}

export interface SheetPortalProps {
  children: React.ReactNode;
  /** Portal target; defaults to document.body. */
  container?: HTMLElement | null;
  forceMount?: boolean;
}

export function SheetPortal({ children, container, forceMount }: SheetPortalProps) {
  const { effectiveOpen } = useSheetContext("SheetPortal");
  if (!(forceMount || effectiveOpen)) return null;
  if (typeof document === "undefined") return null;
  return createPortal(
    <SheetPortalContext.Provider value={true}>{children}</SheetPortalContext.Provider>,
    container ?? document.body,
  );
}

export interface SheetOverlayProps extends React.HTMLAttributes<HTMLDivElement> {
  forceMount?: boolean | undefined;
}

export function SheetOverlay({ forceMount, className, ...props }: SheetOverlayProps) {
  const { effectiveOpen, ids, aria, classes, setPart } = useSheetContext("SheetOverlay");
  if (!(forceMount || effectiveOpen)) return null;
  return (
    <div
      data-part="overlay"
      id={ids.overlay || undefined}
      ref={setPart("overlay")}
      // A force-mounted closed overlay must not cover the page.
      hidden={effectiveOpen ? undefined : true}
      className={classy(classes.overlay, className)}
      {...aria.overlay}
      {...props}
    />
  );
}

export interface SheetTriggerProps extends React.ButtonHTMLAttributes<HTMLButtonElement> {
  asChild?: boolean;
}

export function SheetTrigger({ asChild, onClick, children, ...props }: SheetTriggerProps) {
  const { effectiveOpen, ids, aria, request, setPart } = useSheetContext("SheetTrigger");

  const handleClick = (event: React.MouseEvent<HTMLButtonElement>) => {
    onClick?.(event);
    request(effectiveOpen ? "close" : "open");
  };

  const partProps = {
    "data-part": "trigger",
    id: ids.trigger,
    ref: setPart("trigger"),
    ...aria.trigger,
    onClick: handleClick,
  };

  if (asChild && React.isValidElement(children)) {
    const childProps = children.props as Record<string, unknown>;
    return React.cloneElement(children, mergeProps(partProps, childProps) as React.Attributes);
  }

  return (
    <button type="button" {...partProps} {...props}>
      {children}
    </button>
  );
}

export interface SheetContentProps extends React.HTMLAttributes<HTMLDivElement> {
  /** The edge the sheet anchors to. Positional decoration only. Default right. */
  side?: SheetSide;
  /** Defaults to true, except inside an explicit <SheetPortal> (oracle
   *  compatibility for Radix-style composition). */
  showCloseButton?: boolean;
  forceMount?: boolean;
  /** Portal target for the automatic portal; defaults to document.body. */
  container?: HTMLElement | null;
  /** Consumer veto: called before Escape closes; preventDefault to keep open. */
  onEscapeKeyDown?: (event: KeyboardEvent) => void;
  /** Consumer veto: called before an outside pointerdown closes. */
  onPointerDownOutside?: (event: Event) => void;
  /** Consumer veto: called alongside onPointerDownOutside (oracle surface). */
  onInteractOutside?: (event: Event) => void;
}

export function SheetContent({
  side = "right",
  showCloseButton,
  forceMount,
  container,
  onEscapeKeyDown,
  onPointerDownOutside,
  onInteractOutside,
  className,
  children,
  onKeyDown,
  ...props
}: SheetContentProps) {
  const { config, state, effectiveOpen, ids, aria, classes, request, setPart, dismissVetoRef } =
    useSheetContext("SheetContent");
  const isInsidePortal = React.useContext(SheetPortalContext);
  // Presence (wave 0-B): keep the content mounted through its exit animation.
  // With no exit animation it releases immediately, so behavior is unchanged.
  const { present, ref: presenceRef } = usePresence(effectiveOpen);

  React.useEffect(() => {
    dismissVetoRef.current = { onPointerDownOutside, onInteractOutside };
    return () => {
      dismissVetoRef.current = null;
    };
  });

  if (!(forceMount || present)) return null;
  if (typeof document === "undefined") return null;

  const modal = config.modal !== false;
  // The sheet oracle (src/old/ui/sheet.tsx: showCloseButton ?? true) ALWAYS
  // rendered the close button, including inside an explicit portal -- matching
  // shadcn, where the close lives unconditionally in SheetContent. Kept as-is;
  // this is the parity floor. (Dialog diverges with `?? !isInsidePortal`.)
  const shouldShowCloseButton = showCloseButton ?? true;

  const handleKeyDown = (event: React.KeyboardEvent<HTMLDivElement>) => {
    onKeyDown?.(event);
    if (event.defaultPrevented) return;
    const action = sheet.keymap(keyInputOf(event), state, "content", config);
    if (!action) return;
    if (action === "close") {
      onEscapeKeyDown?.(event.nativeEvent);
      if (event.nativeEvent.defaultPrevented) return;
    }
    event.preventDefault();
    request(action);
  };

  const content = (
    // forceMount keeps the nodes for animation tooling; a closed modal must
    // still be invisible to AT, untabbable, and must not block the page --
    // hidden covers all three.
    <div
      data-part="content"
      id={ids.content || undefined}
      ref={presenceRef}
      tabIndex={-1}
      hidden={present ? undefined : true}
      className={classy(sheetContentClasses(side), className)}
      {...aria.content}
      onKeyDown={handleKeyDown}
      {...props}
    >
      {children}
      {shouldShowCloseButton ? (
        <button
          type="button"
          data-part="close"
          id={ids.close || undefined}
          ref={setPart("close")}
          className={classes.close}
          {...aria.close}
          onClick={() => request("close")}
        >
          <svg
            xmlns="http://www.w3.org/2000/svg"
            viewBox="0 0 24 24"
            fill="none"
            stroke="currentColor"
            strokeWidth="2"
            strokeLinecap="round"
            strokeLinejoin="round"
            className={classes.closeIcon}
            aria-hidden="true"
          >
            <path d="M18 6 6 18" />
            <path d="m6 6 12 12" />
          </svg>
        </button>
      ) : null}
    </div>
  );

  // Inside an explicit <SheetPortal>: the consumer owns portal + overlay.
  if (isInsidePortal) return content;

  // shadcn-style: Content brings its own portal and overlay.
  return createPortal(
    <>
      {modal ? <SheetOverlay forceMount={forceMount} /> : null}
      {content}
    </>,
    container ?? document.body,
  );
}

export type SheetHeaderProps = React.HTMLAttributes<HTMLDivElement>;

export function SheetHeader({ className, ...props }: SheetHeaderProps) {
  const { classes } = useSheetContext("SheetHeader");
  return <div className={classy(classes.header, className)} {...props} />;
}

export type SheetFooterProps = React.HTMLAttributes<HTMLDivElement>;

export function SheetFooter({ className, ...props }: SheetFooterProps) {
  const { classes } = useSheetContext("SheetFooter");
  return <div className={classy(classes.footer, className)} {...props} />;
}

export type SheetTitleProps = React.HTMLAttributes<HTMLHeadingElement>;

export function SheetTitle({ className, ...props }: SheetTitleProps) {
  const { ids, classes, setPart } = useSheetContext("SheetTitle");
  return (
    <h2
      data-part="title"
      id={ids.title || undefined}
      ref={setPart("title")}
      className={classy(classes.title, className)}
      {...props}
    />
  );
}

export type SheetDescriptionProps = React.HTMLAttributes<HTMLParagraphElement>;

export function SheetDescription({ className, ...props }: SheetDescriptionProps) {
  const { ids, classes, setPart } = useSheetContext("SheetDescription");
  return (
    <p
      data-part="description"
      id={ids.description || undefined}
      ref={setPart("description")}
      className={classy(classes.description, className)}
      {...props}
    />
  );
}

export interface SheetCloseProps extends React.ButtonHTMLAttributes<HTMLButtonElement> {
  asChild?: boolean;
}

export function SheetClose({ asChild, onClick, children, ...props }: SheetCloseProps) {
  const { request } = useSheetContext("SheetClose");

  const handleClick = (event: React.MouseEvent<HTMLButtonElement>) => {
    onClick?.(event);
    request("close");
  };

  if (asChild && React.isValidElement(children)) {
    const childProps = children.props as Record<string, unknown>;
    return React.cloneElement(
      children,
      mergeProps({ onClick: handleClick }, childProps) as React.Attributes,
    );
  }

  return (
    <button type="button" onClick={handleClick} {...props}>
      {children}
    </button>
  );
}
