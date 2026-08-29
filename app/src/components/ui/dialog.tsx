/**
 * Modal dialog component with focus management and escape patterns
 *
 * @cognitive-load 6/10 - Interrupts user flow, requires decision making
 * @attention-economics Attention capture: modal=full attention, drawer=partial attention, popover=contextual attention
 * @trust-building Clear close mechanisms, confirmation for destructive actions, non-blocking for informational content
 * @accessibility Focus trapping, escape key handling, backdrop dismissal, screen reader announcements
 * @semantic-meaning Usage patterns: modal=blocking workflow, drawer=supplementary, alert=urgent information
 *
 * @usage-patterns
 * DO: Low trust - Quick confirmations, save draft (size=sm, minimal friction)
 * DO: Medium trust - Publish content, moderate consequences (clear context)
 * DO: High trust - Payments, significant impact (detailed explanation)
 * DO: Critical trust - Account deletion, permanent loss (progressive confirmation)
 * NEVER: Routine actions, non-essential interruptions
 *
 * @example
 * ```tsx
 * // Minimal usage - Portal, Overlay, and Close button are included automatically
 * <Dialog>
 *   <DialogTrigger>Open</DialogTrigger>
 *   <DialogContent>
 *     <DialogHeader>
 *       <DialogTitle>Title</DialogTitle>
 *     </DialogHeader>
 *     Content here
 *   </DialogContent>
 * </Dialog>
 *
 * // Or with namespace syntax
 * <Dialog>
 *   <Dialog.Trigger asChild>
 *     <Button>Open Dialog</Button>
 *   </Dialog.Trigger>
 *   <Dialog.Content>
 *     <Dialog.Header>
 *       <Dialog.Title>Dialog Title</Dialog.Title>
 *       <Dialog.Description>Dialog description here.</Dialog.Description>
 *     </Dialog.Header>
 *   </Dialog.Content>
 * </Dialog>
 *
 * // Hide close button if needed
 * <DialogContent showCloseButton={false}>...</DialogContent>
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
  dialog,
  isOpen,
  startDialogModalEffects,
  type DialogActions,
  type DialogConfig,
  type DialogPart,
  type DialogState,
} from "@/components/ui/dialog.behavior";
import { dialogClasses, type DialogClassSet } from "@/components/ui/dialog.classes";

/** The oracle-compatible dismissal veto surface on DialogContent. */
interface DismissVetoCallbacks {
  onPointerDownOutside?: ((event: Event) => void) | undefined;
  onInteractOutside?: ((event: Event) => void) | undefined;
}

interface DialogContextValue {
  state: DialogState;
  ids: PartIds<DialogPart>;
  aria: Partial<Record<DialogPart, AriaAttrs>>;
  request: (action: keyof DialogActions) => boolean;
  setPart: (part: DialogPart) => (element: HTMLElement | null) => void;
  getPart: (part: string) => HTMLElement | null;
  config: DialogConfig;
  effectiveOpen: boolean;
  classes: DialogClassSet;
  dismissVetoRef: React.RefObject<DismissVetoCallbacks | null>;
}

const DialogContext = React.createContext<DialogContextValue | null>(null);

function useDialogContext(component: string): DialogContextValue {
  const context = React.useContext(DialogContext);
  if (!context) {
    throw new Error(`${component} must be used within <Dialog>`);
  }
  return context;
}

/** True when rendering inside an explicit <DialogPortal> (Radix-style
 *  composition); DialogContent then skips its automatic portal + overlay. */
const DialogPortalContext = React.createContext(false);

export interface DialogProps {
  children: React.ReactNode;
  open?: boolean;
  defaultOpen?: boolean;
  onOpenChange?: (open: boolean) => void;
  modal?: boolean;
}

export function Dialog({
  children,
  open,
  defaultOpen = false,
  onOpenChange,
  modal = true,
}: DialogProps) {
  const config: DialogConfig = { open, defaultOpen, modal };
  const dismissVetoRef = React.useRef<DismissVetoCallbacks | null>(null);

  // The controller composes the score with the substrate -- no useBehavior.
  const { memory, dispatch } = React.useMemo(() => createBehavior(dialog, config), []);
  const state = useMemory(memory);
  const effectiveOpen = isOpen(state, config);

  const uid = React.useId();

  // Content portals to document.body with a unique id, so getPart resolves by
  // id -- no ref registry. Optional parts still need a mount signal (setPart)
  // purely so an omitted description projects no dangling aria-describedby.
  const [presentParts, setPresentParts] = React.useState<ReadonlySet<string>>(new Set());
  const partCallbacks = React.useRef<Map<string, (el: HTMLElement | null) => void>>(new Map());
  const setPart = React.useCallback((part: DialogPart) => {
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
    const out = {} as PartIds<DialogPart>;
    for (const part of Object.keys(dialog.parts) as DialogPart[]) {
      const crossRefSource = part === "title" || part === "description";
      out[part] = crossRefSource && !presentParts.has(part) ? "" : `${uid}-${part}`;
    }
    return out;
  }, [uid, presentParts]);

  const latest = React.useRef({ config, onOpenChange });
  latest.current = { config, onOpenChange };
  const request = React.useCallback(
    (action: keyof DialogActions): boolean => {
      const { config: cfg, onOpenChange: cb } = latest.current;
      if (!dispatch(action, cfg)) return false;
      cb?.(action === "open");
      return true;
    },
    [dispatch],
  );

  // The modal overlay trio, composed directly on the open+modal transition
  // (replacing the effects runner). Level-triggered via the dependency array;
  // the cleanup tears the trio down (focus restore rides the trap teardown).
  React.useEffect(() => {
    if (!effectiveOpen || !modal) return;
    const content = getPart("content");
    if (!content) return;
    return startDialogModalEffects({
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

  const aria = dialog.aria(state, config, ids);

  const contextValue: DialogContextValue = {
    state,
    ids,
    aria,
    request,
    setPart,
    getPart,
    config,
    effectiveOpen,
    classes: dialogClasses(config, state),
    dismissVetoRef,
  };

  return <DialogContext.Provider value={contextValue}>{children}</DialogContext.Provider>;
}

export interface DialogPortalProps {
  children: React.ReactNode;
  /** Portal target; defaults to document.body. */
  container?: HTMLElement | null;
  forceMount?: boolean;
}

export function DialogPortal({ children, container, forceMount }: DialogPortalProps) {
  const { effectiveOpen } = useDialogContext("DialogPortal");
  if (!(forceMount || effectiveOpen)) return null;
  if (typeof document === "undefined") return null;
  return createPortal(
    <DialogPortalContext.Provider value={true}>{children}</DialogPortalContext.Provider>,
    container ?? document.body,
  );
}

export interface DialogOverlayProps extends React.HTMLAttributes<HTMLDivElement> {
  forceMount?: boolean | undefined;
}

export function DialogOverlay({ forceMount, className, ...props }: DialogOverlayProps) {
  const { effectiveOpen, ids, aria, classes, setPart } = useDialogContext("DialogOverlay");
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

export interface DialogTriggerProps extends React.ButtonHTMLAttributes<HTMLButtonElement> {
  asChild?: boolean;
}

export function DialogTrigger({ asChild, onClick, children, ...props }: DialogTriggerProps) {
  const { effectiveOpen, ids, aria, request, setPart } = useDialogContext("DialogTrigger");

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

export interface DialogContentProps extends React.HTMLAttributes<HTMLDivElement> {
  /** Defaults to true, except inside an explicit <DialogPortal> (oracle
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

export function DialogContent({
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
}: DialogContentProps) {
  const { config, state, effectiveOpen, ids, aria, classes, request, setPart, dismissVetoRef } =
    useDialogContext("DialogContent");
  const isInsidePortal = React.useContext(DialogPortalContext);
  // Presence: keep the content mounted through its exit animation. `data-state`
  // is NOT set here -- disclosable already contributes it through `aria.content`,
  // from `isOpen(state, config)`, which is the same value `effectiveOpen` carries
  // into presence. One attribute, one writer. See the ownership note in
  // use-presence.ts.
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
  const shouldShowCloseButton = showCloseButton ?? !isInsidePortal;

  const handleKeyDown = (event: React.KeyboardEvent<HTMLDivElement>) => {
    onKeyDown?.(event);
    if (event.defaultPrevented) return;
    const action = dialog.keymap(keyInputOf(event), state, "content", config);
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
    // hidden on the fixed-position container covers all three.
    <div className={classes.container} hidden={present ? undefined : true}>
      <div
        data-part="content"
        id={ids.content || undefined}
        ref={presenceRef}
        // Inert, not hidden, for the exit window (the ratified ruling). The node
        // must keep rendering so the exit keyframe paints, but a closing dialog
        // is already semantically gone: inert takes it out of the a11y tree, the
        // tab order, and hit-testing without the display:none that would kill the
        // animation. It lifts the moment the dialog reopens.
        inert={effectiveOpen ? undefined : true}
        tabIndex={-1}
        className={classy(classes.content, className)}
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
    </div>
  );

  // Inside an explicit <DialogPortal>: the consumer owns portal + overlay.
  if (isInsidePortal) return content;

  // shadcn-style: Content brings its own portal and overlay.
  return createPortal(
    <>
      {modal ? <DialogOverlay forceMount={forceMount} /> : null}
      {content}
    </>,
    container ?? document.body,
  );
}

export type DialogHeaderProps = React.HTMLAttributes<HTMLDivElement>;

export function DialogHeader({ className, ...props }: DialogHeaderProps) {
  const { classes } = useDialogContext("DialogHeader");
  return <div className={classy(classes.header, className)} {...props} />;
}

export type DialogFooterProps = React.HTMLAttributes<HTMLDivElement>;

export function DialogFooter({ className, ...props }: DialogFooterProps) {
  const { classes } = useDialogContext("DialogFooter");
  return <div className={classy(classes.footer, className)} {...props} />;
}

export type DialogTitleProps = React.HTMLAttributes<HTMLHeadingElement>;

export function DialogTitle({ className, ...props }: DialogTitleProps) {
  const { ids, classes, setPart } = useDialogContext("DialogTitle");
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

export type DialogDescriptionProps = React.HTMLAttributes<HTMLParagraphElement>;

export function DialogDescription({ className, ...props }: DialogDescriptionProps) {
  const { ids, classes, setPart } = useDialogContext("DialogDescription");
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

export interface DialogCloseProps extends React.ButtonHTMLAttributes<HTMLButtonElement> {
  asChild?: boolean;
}

export function DialogClose({ asChild, onClick, children, ...props }: DialogCloseProps) {
  const { request } = useDialogContext("DialogClose");

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
