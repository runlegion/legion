import { compose, type Slice } from "@/lib/compose";
import {
  createBehavior,
  type AriaAttrs,
  type BehaviorSpec,
  type InstanceIds,
  type PartIds,
} from "@/lib/contract";
import { updateAriaAttribute } from "@/lib/primitives/aria-manager";
import { onPointerDownOutside } from "@/lib/primitives/outside-click";
import { createRovingFocus } from "@/lib/primitives/roving-focus";

/**
 * Navigation menu: a bar of triggers, each disclosing a content panel, one
 * open at a time (or none). Replaces the imperative
 * old/ui/navigation-menu.controller.ts wholesale.
 *
 * Focus movement across triggers is NOT state here -- it is ephemeral DOM
 * state owned by the roving-focus effect. The score's only state axis is
 * which item is open.
 */
export interface NavigationMenuConfig {
  /** Controlled open item ('' = none). */
  value?: string | undefined;
  /** Uncontrolled seed. */
  defaultValue?: string | undefined;
  orientation?: "horizontal" | "vertical" | undefined;
}

export interface NavigationMenuState {
  active: string | null;
  /** The open item was opened by hover. The click that lands right after a
   *  hover-open (pointerenter precedes pointerdown in the same gesture)
   *  must not close what the hover just opened; toggle absorbs exactly one
   *  such click. The oracle closed on that click -- a live defect. */
  pointerOpened: boolean;
}

export type NavigationMenuActions = {
  /** Open or switch to an item deliberately (payload: item value). */
  open: string;
  /** Open or switch via hover intent (payload: item value). */
  hoverOpen: string;
  /** Toggle an item (payload: item value). */
  toggle: string;
  /** Close whatever is open. */
  close: undefined;
};

export type NavigationMenuPart = "root" | "list" | "trigger" | "content" | "viewport" | "indicator";

/** The effective open item: controlled value shadows intrinsic state. */
export function activeItem(
  state: NavigationMenuState,
  config: NavigationMenuConfig,
): string | null {
  if (config.value !== undefined) return config.value === "" ? null : config.value;
  return state.active;
}

function orientationOf(config: NavigationMenuConfig): "horizontal" | "vertical" {
  return config.orientation ?? "horizontal";
}

const navigation: Slice<
  NavigationMenuConfig,
  NavigationMenuState,
  NavigationMenuActions,
  NavigationMenuPart
> = {
  name: "navigation",
  parts: {
    root: {},
    list: {},
    trigger: { many: true },
    // Content stays in the DOM hidden when closed: navigation links must be
    // crawlable and SSR-stable. Presence is constant; visibility is state.
    content: { many: true },
    // Decorative chrome (shadcn surface); present while open.
    viewport: { optional: true },
    indicator: { optional: true },
  },
  initialState: (config) => {
    const seed = config.value ?? config.defaultValue ?? "";
    return { active: seed === "" ? null : seed, pointerOpened: false };
  },
  actions: {
    open: (state, value) => ({ ...state, active: value, pointerOpened: false }),
    hoverOpen: (state, value) => ({ ...state, active: value, pointerOpened: true }),
    toggle: (state, value) => {
      if (state.active === value && state.pointerOpened) {
        return { ...state, pointerOpened: false };
      }
      return { ...state, active: state.active === value ? null : value, pointerOpened: false };
    },
    close: (state) => ({ ...state, active: null, pointerOpened: false }),
  },
  canDispatch: (state, action, config) =>
    action === "close" ? activeItem(state, config) !== null : true,
  aria: (state, config, _ids) => {
    const open = activeItem(state, config) !== null;
    return {
      root: {
        "aria-label": "Main navigation",
        "data-orientation": orientationOf(config),
        "data-state": open ? "open" : "closed",
      },
      list: {
        "data-orientation": orientationOf(config),
      },
      viewport: {
        "data-state": open ? "open" : "closed",
        "aria-hidden": open ? undefined : "true",
      },
      indicator: {
        "data-state": open ? "visible" : "hidden",
        "aria-hidden": "true",
      },
    };
  },
  keymap: (event, _state, part, config) => {
    if (event.key === "Escape") return "close";
    // ArrowDown opens the focused trigger only when the roving axis is
    // horizontal; on a vertical axis the roving-focus effect owns ArrowDown.
    if (part === "trigger" && event.key === "ArrowDown" && orientationOf(config) !== "vertical") {
      return "open";
    }
    // Enter/Space -> toggle is the contract; native <button> triggers
    // fulfill it via click (Spec 01 rule 5).
    if (part === "trigger" && (event.key === "Enter" || event.key === " ")) return "toggle";
    return null;
  },
};

/**
 * Per-instance ARIA for the `many` parts (Spec 01: BehaviorSpec.instanceAria).
 * `aria()` projects one AriaAttrs per part NAME; trigger/content occur once per
 * item value, so their projections take the instance value and the instance's
 * sibling ids (a trigger reads its content's id; content reads its trigger's).
 * The generic contract member `navigationMenu.instanceAria` points at this
 * exact function, so the conformance harness drives it without bespoke wiring
 * while the component's own decorators call the concrete function directly.
 */
export function navInstanceAria(
  part: NavigationMenuPart,
  value: string,
  state: NavigationMenuState,
  config: NavigationMenuConfig,
  ids: InstanceIds<NavigationMenuPart>,
): AriaAttrs {
  const open = activeItem(state, config) === value;
  if (part === "trigger") {
    return {
      "aria-expanded": open ? "true" : "false",
      "aria-controls": ids.content ?? "",
      "data-state": open ? "open" : "closed",
    };
  }
  if (part === "content") {
    // NO `hidden` (#2148). `hidden` is UA-stylesheet `display: none`: it pulls
    // the panel out of the accessibility tree, out of rendering, and out of
    // reach of the `:hover` / `:focus-within` reveal the stylesheet now owns.
    // Visibility is opacity + pointer-events, keyed off this same `data-state`
    // and off the item's hover -- see navigation-menu.classes.ts.
    return {
      "aria-labelledby": ids.trigger ?? "",
      "data-state": open ? "open" : "closed",
    };
  }
  return {};
}

export const navigationMenu: BehaviorSpec<
  NavigationMenuConfig,
  NavigationMenuState,
  NavigationMenuActions,
  NavigationMenuPart
> = { ...compose("navigation-menu", navigation), instanceAria: navInstanceAria };

/** The parts, orientation, and dispatch the roving/hover/dismiss trio composes
 *  against. There is no `delay` port any more (#2148): hover-intent timing is
 *  `transition-delay` in navigation-menu.classes.ts, not a number JavaScript
 *  carries. */
export interface NavigationMenuEffectPorts {
  /** The composite root: menubar hover/focus tracking listens here and an
   *  outside pointerdown landing beyond it dismisses. */
  root: HTMLElement;
  /** The trigger list: roving tabindex moves focus across its items. Absent
   *  markup simply skips roving. */
  list: HTMLElement | null;
  orientation: "horizontal" | "vertical";
  /** Open (or hover-switch to) the trigger carrying this value. */
  onHoverOpen: (value: string) => void;
  /** Close whatever is open. */
  onClose: () => void;
}

const TRIGGER_SELECTOR = '[data-part="trigger"]:not([disabled])';
const ITEM_SELECTOR = '[data-part="trigger"], [data-part="content"]';

/**
 * The WCAG 1.4.13 dismissal flag, shared by every performance (#2148).
 *
 * Escape raises it and the stylesheet force-hides the panel on it; a deliberate
 * reopen (click, Enter/Space, ArrowDown) and the pointer or focus leaving the
 * menu clear it. Without it Escape would not visually close anything: dismissal
 * returns focus to the trigger, so the item's `:focus-within` still matches --
 * and `:hover` still matches too, if the pointer never moved. A CSS reveal
 * cannot remember that a dismissal happened; this attribute is that memory.
 *
 * IT IS ONE PANEL'S MEMORY, NOT THE BAR'S. Raised on the root it blanked every
 * panel at once (the force-hide was a descendant rule) and the hover guard below
 * refused every trigger, so after an Escape the whole nav went inert until the
 * pointer left it -- hovering a SIBLING trigger opened nothing, in CSS and in
 * JavaScript alike. The dismissed panel is the thing that was dismissed, so the
 * dismissed panel is where the flag lives.
 */
export function setNavigationMenuDismissed(panel: HTMLElement | null, dismissed: boolean): void {
  if (!panel) return;
  if (dismissed) panel.dataset["dismissed"] = "true";
  else delete panel.dataset["dismissed"];
}

/** The panel belonging to one item value -- where that item's dismissal lives. */
export function navigationMenuPanel(root: HTMLElement | null, value: string): HTMLElement | null {
  return root?.querySelector<HTMLElement>(`[data-part="content"][data-value="${value}"]`) ?? null;
}

/**
 * The item that scopes one panel's reveal: trigger and content are siblings
 * inside it (navigation-menu.classes.ts's `group/navigation-item`), so the
 * item's `:hover` and `:focus-within` are exactly the two conditions the
 * stylesheet reveals the panel on.
 */
const itemScopeOf = (panel: HTMLElement): HTMLElement => panel.parentElement ?? panel;

/**
 * Can this panel still be revealed by CSS alone -- is its item hovered, or does
 * it hold the focus? A dismissal may only be dropped once the answer is no.
 *
 * `focusTarget` is where focus is COMING TO REST, which is not always
 * `document.activeElement`: inside a focusout handler the browser has not moved
 * it yet, so the caller passes the event's relatedTarget instead. Reading
 * activeElement there would call a panel settled at the exact moment Escape is
 * handing focus back to its own trigger.
 */
function canStillReveal(panel: HTMLElement, focusTarget: Node | null): boolean {
  const item = itemScopeOf(panel);
  return item.matches(":hover") || (focusTarget !== null && item.contains(focusTarget));
}

/**
 * Drop every dismissal that has SETTLED -- every flagged panel whose item is
 * neither hovered nor holding focus any more.
 *
 * The condition is the whole point. Clearing a dismissal unconditionally when
 * the pointer left the bar re-revealed the panel the user had just dismissed:
 * a click-to-close or an Escape leaves focus on the trigger (Escape returns it
 * there deliberately), so `group-focus-within/navigation-item:opacity-100` was
 * still matching, and deleting `data-dismissed` brought the panel back at
 * opacity 1 and hit-testable against `data-state="closed"` and
 * `aria-expanded="false"` -- a WCAG 1.4.13 dismissibility failure the `hidden`
 * panel of the old design could not have. The mirror case is focus leaving
 * while the pointer still rests on the item.
 *
 * Because the pointer axis and the focus axis are checked per panel rather than
 * per menu, the two leave handlers hand off to each other: whichever leaves
 * LAST is the one whose sweep finds the panel settled and clears it. Nothing
 * stays flagged once nothing can reveal it, so there is no dead item left
 * behind either.
 */
export function clearSettledNavigationMenuDismissals(
  root: HTMLElement | null,
  focusTarget: Node | null = document.activeElement,
): void {
  if (!root) return;
  for (const panel of root.querySelectorAll<HTMLElement>('[data-part="content"][data-dismissed]')) {
    if (!canStillReveal(panel, focusTarget)) delete panel.dataset["dismissed"];
  }
}

const isDismissed = (panel: HTMLElement | null): boolean => panel?.dataset["dismissed"] === "true";

/**
 * The navigation-menu DOM trio, composed directly from the primitives: roving
 * tabindex across the trigger list, pointer/focus tracking over the root, and
 * outside-pointerdown dismissal. Level-triggered whenever the menu is mounted --
 * BOTH the DOM-native bindNavigationMenu and the React NavigationMenu start this
 * once and call the returned cleanup on unmount. The outside-dismiss listener
 * stays attached throughout; `close` is idempotence-gated (canDispatch rejects
 * it when nothing is open), so onClose acts only while a panel is open -- no
 * per-open re-composition. Cleanup releases LIFO.
 *
 * NO TIMERS (#2148). Hover and focus dispatch IMMEDIATELY, so `aria-expanded`
 * and `data-state` track real interaction for assistive technology while the
 * only hover-intent timing on the page is the panel's `transition-delay`. It is
 * expected and correct for the ARIA state to move before the CSS-delayed visual
 * transition finishes on open; the close carries no delay at all, per the
 * matrix.
 *
 * `pointerenter` is captured (it does not bubble) so one root listener sees
 * every trigger. `pointerleave` is bound on the ROOT ITSELF, not per trigger:
 * travelling from a trigger down onto its own panel must not read as leaving,
 * and there is no linger on this component's close to forgive the flicker if it
 * did.
 */
export function startNavigationMenuEffects({
  root,
  list,
  orientation,
  onHoverOpen,
  onClose,
}: NavigationMenuEffectPorts): () => void {
  const releaseRoving = list ? createRovingFocus(list, { orientation }) : () => {};

  // One entry point for both hover and focus: the item that was dismissed stays
  // dismissed while the pointer/focus sits on it, and reaching any OTHER item
  // opens that one. Reaching a sibling does NOT force the standing flag off the
  // dismissed panel -- Escape left focus on ITS trigger, so its
  // `:focus-within` is still matching and an unconditional clear would bring
  // the dismissed panel back alongside the one being hovered. The sweep drops
  // it the moment it has settled instead. Guarding per item is what keeps a
  // dismissal from making the rest of the bar inert.
  const enterItem = (value: string) => {
    if (isDismissed(navigationMenuPanel(root, value))) return;
    clearSettledNavigationMenuDismissals(root);
    onHoverOpen(value);
  };

  const onPointerEnter = (event: Event) => {
    const value = (event.target as HTMLElement).closest<HTMLElement>(TRIGGER_SELECTOR)?.dataset[
      "value"
    ];
    if (value) enterItem(value);
  };
  const onPointerLeave = () => {
    clearSettledNavigationMenuDismissals(root);
    onClose();
  };
  // focusin/focusout rather than focus/blur: they carry a relatedTarget, which
  // is what tells "focus moved from the trigger into its own panel" (stay open)
  // apart from "focus left the menu" (close).
  const onFocusIn = (event: FocusEvent) => {
    // Escape returns focus to the trigger, which fires focusin. Reopening there
    // would undo the dismissal the user just asked for -- hence the per-item
    // guard inside enterItem.
    const value = (event.target as HTMLElement).closest<HTMLElement>(ITEM_SELECTOR)?.dataset[
      "value"
    ];
    if (value) enterItem(value);
  };
  const onFocusOut = (event: FocusEvent) => {
    // Focus moving off an item settles that item whatever the destination --
    // including a rove to a SIBLING trigger, which never reaches the close
    // below. A flag left standing on a panel nothing can reveal any more is a
    // dead item: the guard in enterItem would keep refusing to open it. The
    // destination is read from relatedTarget, not activeElement, because the
    // browser has not moved focus yet when focusout fires.
    const next = event.relatedTarget;
    clearSettledNavigationMenuDismissals(root, next instanceof Node ? next : null);
    if (next instanceof Node && root.contains(next)) return;
    onClose();
  };

  root.addEventListener("pointerenter", onPointerEnter, true);
  root.addEventListener("pointerleave", onPointerLeave);
  root.addEventListener("focusin", onFocusIn);
  root.addEventListener("focusout", onFocusOut);

  const releaseDismiss = onPointerDownOutside(root, () => {
    onClose();
  });
  return () => {
    releaseDismiss();
    root.removeEventListener("focusout", onFocusOut);
    root.removeEventListener("focusin", onFocusIn);
    root.removeEventListener("pointerleave", onPointerLeave);
    root.removeEventListener("pointerenter", onPointerEnter, true);
    releaseRoving();
  };
}

/**
 * The DOM-native binding of the score -- the client. The Web Component and
 * the Astro <script> both import THIS; only React (retained-mode) reads the
 * projections above declaratively instead. Composes the substrate the same
 * way the React controller does: createBehavior is the model,
 * startNavigationMenuEffects composes the roving/hover/dismiss primitives
 * directly, aria-manager applies the projection, and the DOM is the part
 * registry. Living here keeps one binding for every DOM-native performance --
 * no per-framework copy, no drift.
 */
export function bindNavigationMenu(root: HTMLElement): () => void {
  // Config travels as `data-*` and nothing else (#2001): `orientation` is not a
  // valid attribute on a <nav>, and only `data-*` reaches `dataset`.
  // `data-orientation` is written by the score's own aria projection (see
  // navigationMenu.aria), so the markup must not re-emit it. There is no
  // delay attribute left to read (#2148).
  const config: NavigationMenuConfig = {
    orientation: root.dataset["orientation"] === "vertical" ? "vertical" : "horizontal",
    defaultValue:
      root.querySelector<HTMLElement>('[data-part="trigger"][data-state="open"]')?.dataset[
        "value"
      ] ?? "",
  };

  const getPart = (part: string): HTMLElement | null =>
    part === "root" ? root : root.querySelector<HTMLElement>(`[data-part="${part}"]`);

  const { memory, dispatch } = createBehavior(navigationMenu, config);

  const request = (action: keyof NavigationMenuActions, payload?: string): boolean =>
    dispatch(action, config, ...((payload === undefined ? [] : [payload]) as [string]));

  // ids are READ from the markup (server- or author-minted), never generated.
  const ids = {} as PartIds<NavigationMenuPart>;
  for (const part of Object.keys(navigationMenu.parts) as NavigationMenuPart[]) {
    ids[part] = getPart(part)?.id ?? "";
  }

  // The score's projection is already resolved (final strings, undefined =
  // absent), so apply it raw: validate:false skips aria-manager's author-input
  // coercion, which would re-interpret the string 'false' as truthy.
  const applyProjection = (el: HTMLElement, attrs: AriaAttrs) => {
    for (const [name, value] of Object.entries(attrs)) {
      updateAriaAttribute(el, name as never, value as never, { validate: false });
    }
  };

  // The `many` parts, resolved once from the score's own declaration -- no
  // hardcoded trigger/content list, so any many-part score binds identically.
  const manyParts = (Object.keys(navigationMenu.parts) as NavigationMenuPart[]).filter(
    (part) => navigationMenu.parts[part].many,
  );

  // Project every rendered instance of every `many` part, resolving each
  // instance's sibling ids (the elements of the many parts sharing its value).
  const projectInstances = (state: NavigationMenuState) => {
    for (const part of manyParts) {
      for (const el of root.querySelectorAll<HTMLElement>(`[data-part="${part}"]`)) {
        const value = el.dataset["value"];
        if (value === undefined) continue;
        const instanceIds: InstanceIds<NavigationMenuPart> = {};
        for (const sibling of manyParts) {
          instanceIds[sibling] =
            root.querySelector<HTMLElement>(`[data-part="${sibling}"][data-value="${value}"]`)
              ?.id ?? "";
        }
        applyProjection(el, navInstanceAria(part, value, state, config, instanceIds));
      }
    }
  };

  const render = () => {
    const state = memory.get();
    const projection = navigationMenu.aria(state, config, ids);
    for (const part of Object.keys(projection) as NavigationMenuPart[]) {
      const attrs = projection[part];
      const el = getPart(part);
      if (el && attrs) applyProjection(el, attrs);
    }
    projectInstances(state);
  };
  const unsubscribe = memory.subscribe(render); // fires immediately: first paint

  // Compose the roving/hover/dismiss trio directly, level-triggered whenever
  // mounted -- one instance for the menu's lifetime; every dispatch is
  // immediate, so there is nothing to re-compose on open/close.
  const stopEffects = startNavigationMenuEffects({
    root,
    list: getPart("list"),
    orientation: orientationOf(config),
    onHoverOpen: (value) => void request("hoverOpen", value),
    onClose: () => void request("close"),
  });

  const onClick = (event: Event) => {
    const trigger = (event.target as HTMLElement).closest<HTMLElement>('[data-part="trigger"]');
    const value = trigger?.dataset["value"];
    if (!value || !root.contains(trigger)) return;
    // A deliberate click is a fresh intent for THIS item, so it drops THIS
    // panel's flag unconditionally -- the settling sweep would refuse to,
    // because the item the user just clicked is both hovered and focused.
    // Siblings keep their own flags; the sweep drops those when they settle.
    setNavigationMenuDismissed(navigationMenuPanel(root, value), false);
    request("toggle", value);
    // ...but a click (or Enter/Space, which a native button fulfils as a click)
    // that CLOSED the panel leaves focus on the trigger, so the item still
    // matches `:focus-within` and the reveal rule would keep the panel visible
    // against a `data-state="closed"`. Same dismissal the Escape path raises,
    // for the same reason -- and on the same one panel.
    if (memory.get().active === null) {
      setNavigationMenuDismissed(navigationMenuPanel(root, value), true);
    }
  };
  root.addEventListener("click", onClick);

  const onKeydown = (event: KeyboardEvent) => {
    const trigger = (event.target as HTMLElement).closest<HTMLElement>('[data-part="trigger"]');
    const action = navigationMenu.keymap(
      {
        key: event.key,
        shiftKey: event.shiftKey,
        ctrlKey: event.ctrlKey,
        altKey: event.altKey,
        metaKey: event.metaKey,
      },
      memory.get(),
      trigger ? "trigger" : "root",
      config,
    );
    if (!action || action === "toggle") return; // native button click dispatches toggle
    if (action === "open") {
      const value = trigger?.dataset["value"];
      if (!value) return;
      event.preventDefault();
      // A deliberate open is a fresh intent for THIS item -- same reason as the
      // click path: the trigger holds focus, so only an unconditional drop gets
      // the panel back up.
      setNavigationMenuDismissed(navigationMenuPanel(root, value), false);
      request("open", value);
      return;
    }
    if (action === "close") {
      const active = activeItem(memory.get(), config);
      if (active === null) return;
      event.preventDefault();
      const openTrigger = root.querySelector<HTMLElement>(
        `[data-part="trigger"][data-value="${active}"]`,
      );
      request("close");
      // Raise the flag on the dismissed panel BEFORE returning focus: the
      // refocus fires focusin, and the guard there reads this attribute to know
      // not to reopen THIS item -- while its siblings stay live.
      setNavigationMenuDismissed(navigationMenuPanel(root, active), true);
      openTrigger?.focus();
    }
  };
  root.addEventListener("keydown", onKeydown);

  return () => {
    unsubscribe();
    stopEffects();
    root.removeEventListener("click", onClick);
    root.removeEventListener("keydown", onKeydown);
  };
}
