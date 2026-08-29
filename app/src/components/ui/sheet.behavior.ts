import { compose, type GlueSlice, type Slice } from "@/lib/compose";
import { createBehavior, type AriaAttrs, type BehaviorSpec, type PartIds } from "@/lib/contract";
import { updateAriaAttribute } from "@/lib/primitives/aria-manager";
import { createFocusTrap, preventBodyScroll } from "@/lib/primitives/focus-trap";
import { onPointerDownOutside } from "@/lib/primitives/outside-click";
import {
  disclosable,
  isOpen,
  type DisclosableActions,
  type DisclosableConfig,
  type DisclosablePart,
  type DisclosableState,
} from "@/lib/disclosable";

/** The edge a sheet is anchored to. Purely positional -- it selects the view's
 *  side variant and the slide axis; it never enters a reducer, projects no
 *  ARIA, and claims no key. Default: right. */
export type SheetSide = "top" | "right" | "bottom" | "left";

export interface SheetConfig extends DisclosableConfig {
  /** Modal sheets trap focus, lock scroll, and dismiss on outside pointerdown.
   *  Default: true. */
  modal?: boolean | undefined;
}

export type SheetState = DisclosableState;
export type SheetActions = DisclosableActions;

export type SheetSurfacePart = "overlay" | "title" | "description" | "close";
export type SheetPart = DisclosablePart | SheetSurfacePart;

export { isOpen };

function isModal(config: SheetConfig): boolean {
  return config.modal !== false;
}

/** Structure-only slice: the parts a sheet has beyond the disclosable
 *  trigger/content pair. Contributes no state and no actions. */
const sheetSurface: Slice<
  SheetConfig,
  Record<never, never>,
  Record<never, never>,
  SheetSurfacePart
> = {
  name: "sheet-surface",
  parts: {
    overlay: { optional: true },
    title: { optional: true },
    description: { optional: true },
    close: { optional: true },
  },
  initialState: () => ({}),
};

/** The sheet glue: ARIA identity and the Escape contract, written over the
 *  merged state. The modal overlay concerns (focus-trap, scroll-lock,
 *  outside-dismiss) are composed directly by the bindings, not declared here.
 *  A sheet is an edge-anchored dialog, so it carries role="dialog". */
const sheetGlue: GlueSlice<SheetConfig, SheetState, { close: undefined }, SheetPart> = {
  kind: "glue",
  name: "sheet",
  aria: (state, config, ids) => {
    const open = isOpen(state, config);
    return {
      trigger: {
        "aria-haspopup": "dialog",
      },
      content: {
        role: "dialog",
        "aria-modal": isModal(config) ? "true" : undefined,
        // An empty id means the binding did not render that part; a reference
        // to a missing id is an axe violation, so project absence.
        "aria-labelledby": ids.title || undefined,
        "aria-describedby": ids.description || undefined,
      },
      overlay: {
        "aria-hidden": "true",
        "data-state": open ? "open" : "closed",
      },
      close: {
        "aria-label": "Close",
      },
    };
  },
  keymap: (event, _state, part) => (part === "content" && event.key === "Escape" ? "close" : null),
};

/** The parts and dispatch the modal overlay trio composes against. */
export interface SheetModalPorts {
  /** The sheet surface: focus is trapped inside it and a pointerdown landing
   *  outside it dismisses. */
  content: HTMLElement;
  /** Resolves the trigger so the opening gesture's pointerdown is spared --
   *  otherwise it would both dismiss the layer and re-open it. */
  getTrigger: () => HTMLElement | null;
  /** Outside-pointerdown handler, already spared of the trigger. Receives the
   *  native event so a boundary can offer a consumer veto before closing. */
  onDismiss: (event: Event) => void;
}

/**
 * The modal overlay trio, composed directly (the retired effects runner's
 * replacement): trap Tab focus inside `content`, lock body scroll, and dismiss
 * on a pointerdown outside `content` -- sparing the trigger. Level-triggered:
 * BOTH the DOM-native bindSheet and the React Sheet start this on the
 * open+modal transition and call the returned cleanup on close/unmount. Focus
 * restore rides the trap teardown, so the cleanup releases LIFO.
 */
export function startSheetModalEffects({
  content,
  getTrigger,
  onDismiss,
}: SheetModalPorts): () => void {
  const releaseTrap = createFocusTrap(content);
  const releaseScroll = preventBodyScroll();
  const releaseDismiss = onPointerDownOutside(content, (event) => {
    const target = event.target as Node;
    if (getTrigger()?.contains(target)) return;
    onDismiss(event);
  });
  return () => {
    releaseDismiss();
    releaseScroll();
    releaseTrap();
  };
}

export const sheet: BehaviorSpec<SheetConfig, SheetState, SheetActions, SheetPart> = compose(
  "sheet",
  disclosable<SheetConfig>(),
  sheetSurface,
  sheetGlue,
);

/**
 * The DOM-native binding of the sheet score -- the client. The Web Component
 * and the Astro <script> both import THIS; only React reads the projections
 * declaratively. Same shape as bindDialog, plus the two overlay concerns:
 * PRESENCE (content/overlay are present-but-hidden, toggled on the open axis --
 * the trapped/dismissable parts must be light DOM so focus-trap's activeElement
 * read and dismiss's document .contains work) and the modal overlay trio
 * (focus-trap, scroll-lock, dismiss-on-outside), composed directly and
 * level-triggered: started on the open+modal transition and torn down on
 * close/unbind. The `side` variant is authored into the markup's classes; it is
 * decoration, so the binding never touches it. Enter-only; exit animation waits
 * on Presence.
 */
export function bindSheet(root: HTMLElement): () => void {
  const config: SheetConfig = {
    modal: root.getAttribute("modal") !== "false",
    defaultOpen:
      root.getAttribute("default-open") === "true" ||
      root.querySelector<HTMLElement>('[data-part="content"]')?.dataset["state"] === "open",
  };

  const getPart = (part: string): HTMLElement | null =>
    part === "root" ? root : root.querySelector<HTMLElement>(`[data-part="${part}"]`);

  const { memory, dispatch } = createBehavior(sheet, config);

  const request = (action: keyof SheetActions): boolean => dispatch(action, config);

  // The modal overlay trio is level-triggered: present only while open+modal.
  // render() starts it on the transition and this cleanup stops it on close.
  let modalCleanup: (() => void) | null = null;

  // ids READ from the server/author markup, never generated.
  const ids = {} as PartIds<SheetPart>;
  for (const part of Object.keys(sheet.parts) as SheetPart[]) ids[part] = getPart(part)?.id ?? "";

  // The projection is already resolved, so apply it raw (validate:false skips
  // aria-manager's author-input coercion that flips the string 'false').
  const applyProjection = (el: HTMLElement, attrs: AriaAttrs) => {
    for (const [name, value] of Object.entries(attrs)) {
      updateAriaAttribute(el, name as never, value as never, { validate: false });
    }
  };

  const render = () => {
    const state = memory.get();
    const open = isOpen(state, config);
    const projection = sheet.aria(state, config, ids);
    for (const part of Object.keys(projection) as SheetPart[]) {
      const attrs = projection[part];
      const el = getPart(part);
      if (el && attrs) applyProjection(el, attrs);
    }
    // Presence: the overlay and the content container hide off the open axis.
    // The parts stay in light DOM (crawlable, and effects can read them).
    for (const part of ["overlay", "content"] as const) {
      const el = getPart(part);
      if (el) el.hidden = !open;
    }
    // Compose the modal overlay trio directly, level-triggered: start it once
    // on the open+modal transition (content is now un-hidden above so the trap
    // can read its focusables), tear it down when it should no longer be present.
    const wantModal = open && isModal(config);
    if (wantModal && !modalCleanup) {
      const content = getPart("content");
      if (content) {
        modalCleanup = startSheetModalEffects({
          content,
          getTrigger: () => getPart("trigger"),
          onDismiss: () => {
            request("close");
          },
        });
      }
    } else if (!wantModal && modalCleanup) {
      modalCleanup();
      modalCleanup = null;
    }
  };
  const unsubscribe = memory.subscribe(render); // fires immediately: first paint

  const onClick = (event: Event) => {
    const target = event.target as HTMLElement;
    if (target.closest('[data-part="close"]')) {
      request("close");
      return;
    }
    if (target.closest('[data-part="trigger"]')) {
      request(isOpen(memory.get(), config) ? "close" : "open");
    }
  };
  root.addEventListener("click", onClick);

  const onKeydown = (event: KeyboardEvent) => {
    const partEl = (event.target as HTMLElement).closest<HTMLElement>("[data-part]");
    const part = partEl?.dataset["part"] as SheetPart | undefined;
    if (!part) return;
    const action = sheet.keymap(
      {
        key: event.key,
        shiftKey: event.shiftKey,
        ctrlKey: event.ctrlKey,
        altKey: event.altKey,
        metaKey: event.metaKey,
      },
      memory.get(),
      part,
      config,
    );
    if (!action) return;
    event.preventDefault();
    const trigger = getPart("trigger");
    request(action);
    if (action === "close") trigger?.focus();
  };
  root.addEventListener("keydown", onKeydown);

  return () => {
    unsubscribe();
    modalCleanup?.();
    modalCleanup = null;
    root.removeEventListener("click", onClick);
    root.removeEventListener("keydown", onKeydown);
  };
}
