import { compose, type GlueSlice, type Slice } from "@/lib/compose";
import { createBehavior, type AriaAttrs, type BehaviorSpec, type PartIds } from "@/lib/contract";
import {
  disclosable,
  isOpen,
  type DisclosableActions,
  type DisclosableConfig,
  type DisclosablePart,
  type DisclosableState,
} from "@/lib/disclosable";
import { updateAriaAttribute } from "@/lib/primitives/aria-manager";
import { onPointerDownOutside } from "@/lib/primitives/outside-click";
import { createRovingFocus } from "@/lib/primitives/roving-focus";
import { createTypeahead } from "@/lib/primitives/typeahead";

/**
 * Dropdown menu: a trigger discloses an anchored action menu. Opening lands
 * focus on the first item; roving-focus moves it with the arrow keys, typeahead
 * jumps to the first matching label, and activating an item runs its action and
 * closes. Replaces the imperative old/ui/dropdown-menu.tsx wholesale.
 *
 * The score's only state axis is `open`. The "highlighted item" is NOT state
 * here -- it is ephemeral DOM focus owned by roving-focus and styled via
 * `:focus`, exactly the stance navigation-menu documents for its trigger focus
 * movement. The oracle carried no `data-highlighted` and no pointer-move-to-
 * focus; select tracks a highlight only because it projects `data-highlighted`
 * keyed by an option `value`, which menu action items do not have.
 *
 * open/close reuse the `disclosable` lib slice (the dialog pattern): a reducer
 * over the ONE createBehavior cell, controlled config shadowing intrinsic state,
 * idempotence-gated so consumer callbacks fire once per real transition.
 */
export type DropdownMenuConfig = DisclosableConfig;
export type DropdownMenuState = DisclosableState;
export type DropdownMenuActions = DisclosableActions;

export type DropdownMenuPart = DisclosablePart | "root" | "item";

export { isOpen };

/** Structure-only slice: the parts a menu has beyond the disclosable
 *  trigger/content pair. Contributes no state and no actions. The `item` part
 *  carries NO `role` in its PartDecl -- checkbox/radio items use
 *  menuitemcheckbox/menuitemradio, so the role is author markup, not forced. */
const menuStructure: Slice<
  DropdownMenuConfig,
  Record<never, never>,
  Record<never, never>,
  "root" | "item"
> = {
  name: "dropdown-menu-structure",
  parts: {
    root: {},
    item: { many: true },
  },
  initialState: () => ({}),
};

/** The menu glue: the trigger's haspopup, the menu role/orientation/name over
 *  the disclosable open axis, and the Escape contract. Roving/typeahead/dismiss
 *  are composed directly by the bindings, not declared here. */
const menuGlue: GlueSlice<
  DropdownMenuConfig,
  DropdownMenuState,
  DisclosableActions,
  DropdownMenuPart
> = {
  kind: "glue",
  name: "dropdown-menu",
  aria: (state, config, ids) => {
    const open = isOpen(state, config);
    return {
      root: {
        "data-state": open ? "open" : "closed",
      },
      trigger: {
        "aria-haspopup": "menu",
      },
      content: {
        role: "menu",
        "aria-orientation": "vertical",
        // The menu is named by the trigger that controls it. An empty id means
        // the trigger part is not rendered; project absence rather than a
        // dangling aria-labelledby (an axe violation).
        "aria-labelledby": ids.trigger || undefined,
      },
    };
  },
  keymap: (event, _state, part, _config) => {
    if (part === "trigger") {
      // A menu button opens on ArrowDown/ArrowUp/Enter/Space; canDispatch drops
      // it when already open. The decorator preventDefaults, suppressing the
      // native button click that Enter/Space would otherwise also fire.
      if (
        event.key === "ArrowDown" ||
        event.key === "ArrowUp" ||
        event.key === "Enter" ||
        event.key === " "
      ) {
        return "open";
      }
      return null;
    }
    // Focus is inside the menu, on an item or the menu container.
    if (part === "content" || part === "item") {
      if (event.key === "Escape") return "close";
    }
    return null;
  },
};

export const dropdownMenu: BehaviorSpec<
  DropdownMenuConfig,
  DropdownMenuState,
  DropdownMenuActions,
  DropdownMenuPart
> = compose("dropdown-menu", disclosable<DropdownMenuConfig>(), menuStructure, menuGlue);

/** The parts and dispatch the open-menu trio composes against. */
export interface DropdownMenuOpenPorts {
  /** The menu: focus roves inside it, typeahead jumps within it, and a
   *  pointerdown landing outside it dismisses. */
  content: HTMLElement;
  /** Resolves the trigger so the opening gesture's pointerdown is spared --
   *  otherwise it would both dismiss the menu and re-open it. */
  getTrigger: () => HTMLElement | null;
  /** Outside-pointerdown handler, already spared of the trigger. Receives the
   *  native event so a boundary could offer a consumer veto before closing. */
  onDismiss: (event: Event) => void;
}

/** The enabled menu items -- the set roving and typeahead operate over, shared
 *  so both agree. All three item roles (menuitem/menuitemcheckbox/
 *  menuitemradio) carry data-part="item", so this globs them uniformly. */
export function enabledItems(content: HTMLElement): HTMLElement[] {
  return Array.from(content.querySelectorAll<HTMLElement>('[data-part="item"]')).filter(
    (item) => !item.hasAttribute("data-disabled") && item.getAttribute("aria-disabled") !== "true",
  );
}

/**
 * The open-menu effect trio, composed directly (replacing the retired effects
 * runner): rove arrow/Home/End focus across the items, type-to-jump to the
 * first matching item, and dismiss on a pointerdown outside `content` --
 * sparing the trigger. Level-triggered: BOTH bindDropdownMenu and the React
 * DropdownMenu start this on the open transition (after content is un-hidden so
 * the item set is focusable) and call the returned cleanup on close/unmount.
 * Cleanup releases LIFO.
 */
export function startDropdownMenuEffects({
  content,
  getTrigger,
  onDismiss,
}: DropdownMenuOpenPorts): () => void {
  const stopRoving = createRovingFocus(content, { orientation: "vertical", loop: true });
  const stopTypeahead = createTypeahead(content, {
    getItems: () => enabledItems(content),
    onMatch: (item) => item.focus(),
  });
  const stopDismiss = onPointerDownOutside(content, (event) => {
    const target = event.target as Node;
    if (getTrigger()?.contains(target)) return;
    onDismiss(event);
  });
  return () => {
    stopDismiss();
    stopTypeahead();
    stopRoving();
  };
}

/** Move focus to the first enabled item -- the open-focus semantic (keyboard
 *  users land on the first action). Shared by the bind and the React decorator
 *  so all three performances behave alike. */
export function focusFirstItem(content: HTMLElement | null): void {
  if (!content) return;
  enabledItems(content)[0]?.focus();
}

/**
 * The DOM-native binding of the dropdown-menu score -- the client. The Web
 * Component and the Astro <script> both import THIS; only React (retained-mode)
 * reads the projections declaratively. Same shape as bindSelect/bindDialog,
 * plus the menu concerns: presence (the menu hides off the open axis, staying
 * in light DOM so the roving/typeahead/dismiss effects can read it), open-focus
 * of the first item, and Enter/Space item activation via the shared click path.
 */
export function bindDropdownMenu(root: HTMLElement): () => void {
  const contentEl = root.querySelector<HTMLElement>('[data-part="content"]');
  const config: DropdownMenuConfig = {
    defaultOpen:
      root.getAttribute("default-open") === "true" || contentEl?.dataset["state"] === "open",
  };

  const getPart = (part: string): HTMLElement | null =>
    part === "root" ? root : root.querySelector<HTMLElement>(`[data-part="${part}"]`);

  const { memory, dispatch } = createBehavior(dropdownMenu, config);

  const request = (action: keyof DropdownMenuActions): boolean => dispatch(action, config);

  // The open-menu trio is level-triggered: present only while open. render()
  // starts it on the transition and this cleanup stops it on close.
  let openEffectsCleanup: (() => void) | null = null;

  // ids READ from the server/author markup, never generated.
  const ids = {} as PartIds<DropdownMenuPart>;
  for (const part of Object.keys(dropdownMenu.parts) as DropdownMenuPart[]) {
    ids[part] = getPart(part)?.id ?? "";
  }

  // Resolved projection: apply raw (validate:false skips aria-manager's
  // author-input coercion that would flip the string 'false' to truthy).
  const applyProjection = (el: HTMLElement, attrs: AriaAttrs) => {
    for (const [name, value] of Object.entries(attrs)) {
      updateAriaAttribute(el, name as never, value as never, { validate: false });
    }
  };

  const render = () => {
    const state = memory.get();
    const open = isOpen(state, config);

    const projection = dropdownMenu.aria(state, config, ids);
    for (const part of Object.keys(projection) as DropdownMenuPart[]) {
      const attrs = projection[part];
      const el = getPart(part);
      if (el && attrs) applyProjection(el, attrs);
    }

    // Presence: the menu hides off the open axis (stays in light DOM).
    if (contentEl) contentEl.hidden = !open;

    // Compose the open-menu trio directly, level-triggered: start it once on the
    // open transition (content is now un-hidden above so roving and typeahead
    // see focusable items), tear it down when the menu closes.
    if (open && !openEffectsCleanup) {
      const content = getPart("content");
      if (content) {
        openEffectsCleanup = startDropdownMenuEffects({
          content,
          getTrigger: () => getPart("trigger"),
          onDismiss: () => {
            request("close");
          },
        });
      }
    } else if (!open && openEffectsCleanup) {
      openEffectsCleanup();
      openEffectsCleanup = null;
    }

    // Open-focus: land on the first item when the menu opens and focus is not
    // already inside it. Once focus is in, subsequent renders see it contained
    // and do not steal it back.
    if (open && contentEl && !contentEl.contains(document.activeElement)) {
      focusFirstItem(contentEl);
    }
  };
  const unsubscribe = memory.subscribe(render); // fires immediately: first paint

  const isDisabledItem = (item: HTMLElement): boolean =>
    item.hasAttribute("data-disabled") || item.getAttribute("aria-disabled") === "true";

  const onClick = (event: Event) => {
    const target = event.target as HTMLElement;
    const item = target.closest<HTMLElement>('[data-part="item"]');
    if (item && root.contains(item)) {
      if (isDisabledItem(item)) return;
      // Activating an item runs its action (author markup / the consumer's own
      // handler) and closes; the score owns the close, focus returns to trigger.
      request("close");
      getPart("trigger")?.focus();
      return;
    }
    const trigger = target.closest<HTMLElement>('[data-part="trigger"]');
    if (trigger && root.contains(trigger)) {
      request(isOpen(memory.get(), config) ? "close" : "open");
    }
  };
  root.addEventListener("click", onClick);

  const onKeydown = (event: KeyboardEvent) => {
    const partEl = (event.target as HTMLElement).closest<HTMLElement>("[data-part]");
    const part = partEl?.dataset["part"] as DropdownMenuPart | undefined;
    if (!part) return;

    // Enter/Space on a menu item activate it -- the div-as-button affordance the
    // oracle spelled out (handleKeyDown === handleClick). The STATE effect
    // (close) rides the single click path; the keymap owns only Escape/open.
    const item = partEl?.closest<HTMLElement>('[data-part="item"]');
    if (item && (event.key === "Enter" || event.key === " ")) {
      if (isDisabledItem(item)) return;
      event.preventDefault();
      item.click();
      return;
    }

    const action = dropdownMenu.keymap(
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
    // preventDefault suppresses the native button click Enter/Space would
    // otherwise fire on the trigger (which would toggle back closed).
    event.preventDefault();
    if (action === "open") {
      request("open");
      return;
    }
    if (action === "close") {
      request("close");
      getPart("trigger")?.focus();
    }
  };
  root.addEventListener("keydown", onKeydown);

  return () => {
    unsubscribe();
    openEffectsCleanup?.();
    openEffectsCleanup = null;
    root.removeEventListener("click", onClick);
    root.removeEventListener("keydown", onKeydown);
  };
}
