import { compose, type Slice } from "@/lib/compose";
import { createBehavior, type AriaAttrs, type BehaviorSpec, type PartIds } from "@/lib/contract";
import { updateAriaAttribute } from "@/lib/primitives/aria-manager";
import { fuzzyMatch } from "@/lib/primitives/command-palette";
import { createFocusTrap, preventBodyScroll } from "@/lib/primitives/focus-trap";
import { onPointerDownOutside } from "@/lib/primitives/outside-click";

/**
 * Command: a filtered command palette. A search combobox narrows a listbox of
 * options; the highlighted option is virtual (aria-activedescendant), so DOM
 * focus never leaves the input and the user keeps typing. Ports the React-only
 * old/ui/command.tsx wholesale.
 *
 * The two state axes are `query` and `highlighted`:
 *  - query is the search string. Controlled/uncontrolled per boundary 4:
 *    config.value shadows state.query; the effective read is queryValue().
 *    Changing the query resets the highlight (ported activeIndex = -1), so a
 *    fresh keystroke never leaves a stale option active.
 *  - highlighted is the option the user is pointed at. Unlike select (which
 *    roves real DOM focus across options) a combobox keeps focus on the input
 *    and points at the active option VIRTUALLY via aria-activedescendant. So
 *    roving-focus -- which moves DOM focus and item tabindex -- is deliberately
 *    NOT composed here; it would pull focus off the search box. highlighted is
 *    a plain reducer value and the navigation math (moveHighlight) is pure over
 *    the ordered set of currently-visible option values, which the bindings
 *    read from the DOM and pass as the action payload.
 *
 * Filtering composes the command-palette primitive's pure `fuzzyMatch` (the
 * createCommandPalette CONTROLLER is cell-owning and editor-specific -- it reads
 * contenteditable selection ranges -- so it does not compose; only the pure
 * matcher does). This upgrades the oracle's substring `includes` filter to
 * fuzzy matching, the behaviour the issue calls for.
 */
export interface CommandConfig {
  /** Controlled search query. */
  value?: string | undefined;
  /** Uncontrolled seed. */
  defaultValue?: string | undefined;
  /** Accessible name for the listbox popup (default 'Suggestions'). */
  label?: string | undefined;
}

export interface CommandState {
  /** Intrinsic search query. */
  query: string;
  /** The option value the user is pointed at (aria-activedescendant target). */
  highlighted: string | undefined;
}

export type CommandActions = {
  /** Set the query and reset the highlight (payload: query). */
  setQuery: string;
  /** Point at an option value (payload: value). */
  highlight: string;
  /** Move to the next visible option (payload: ordered visible values). */
  highlightNext: string[];
  /** Move to the previous visible option (payload: ordered visible values). */
  highlightPrev: string[];
  /** Jump to the first visible option (payload: ordered visible values). */
  highlightFirst: string[];
  /** Jump to the last visible option (payload: ordered visible values). */
  highlightLast: string[];
  /** Commit an option: point at it; the bindings fire the consumer select
   *  (payload: value). */
  select: string;
};

export type CommandPart = "root" | "input" | "list" | "item" | "empty";

/** The effective query: controlled config shadows intrinsic state. */
export function queryValue(state: CommandState, config: CommandConfig): string {
  return config.value ?? state.query;
}

/** Whether an option's searchable text matches the query. An empty query
 *  matches everything; otherwise the command-palette primitive's fuzzyMatch
 *  decides (case-insensitive, prefix/consecutive scored). */
export function matchesQuery(text: string, query: string): boolean {
  if (query === "") return true;
  return fuzzyMatch(text, query).matches;
}

/** The next highlight given the ordered visible option values, the current
 *  highlight, and a direction. Clamp, never wrap (ported from the oracle's
 *  min/max on activeIndex): the reason roving-focus -- which wraps -- is wrong
 *  for a combobox. Undefined highlight reads as index -1, so 'next' lands on the
 *  first option and 'prev' clamps to the first. */
export function moveHighlight(
  visible: string[],
  current: string | undefined,
  direction: "next" | "prev" | "first" | "last",
): string | undefined {
  if (visible.length === 0) return undefined;
  const index = current === undefined ? -1 : visible.indexOf(current);
  switch (direction) {
    case "next":
      return visible[Math.min(index + 1, visible.length - 1)];
    case "prev":
      return visible[Math.max(index - 1, 0)];
    case "first":
      return visible[0];
    case "last":
      return visible[visible.length - 1];
  }
}

const commandSlice: Slice<CommandConfig, CommandState, CommandActions, CommandPart> = {
  name: "command",
  parts: {
    root: {},
    input: { role: "combobox" },
    list: { role: "listbox" },
    item: { many: true, role: "option" },
    // The empty state is present-but-hidden; the bindings toggle its visibility
    // from the live match count, which the score cannot know (it has no item
    // set), so it carries no aria projection.
    empty: { optional: true },
  },
  initialState: (config) => ({
    query: config.value ?? config.defaultValue ?? "",
    highlighted: undefined,
  }),
  actions: {
    // A fresh query resets the highlight (ported activeIndex = -1): a keystroke
    // never leaves a stale option active-descendant.
    setQuery: (_state, query) => ({ query, highlighted: undefined }),
    highlight: (state, value) => ({ ...state, highlighted: value }),
    highlightNext: (state, visible) => ({
      ...state,
      highlighted: moveHighlight(visible, state.highlighted, "next"),
    }),
    highlightPrev: (state, visible) => ({
      ...state,
      highlighted: moveHighlight(visible, state.highlighted, "prev"),
    }),
    highlightFirst: (state, visible) => ({
      ...state,
      highlighted: moveHighlight(visible, state.highlighted, "first"),
    }),
    highlightLast: (state, visible) => ({
      ...state,
      highlighted: moveHighlight(visible, state.highlighted, "last"),
    }),
    // Commit points at the option; the bindings fire the consumer select and
    // emit the command-select event. State-wise this only settles the highlight.
    select: (state, value) => ({ ...state, highlighted: value }),
  },
  canDispatch: () => true,
  aria: (_state, config, ids) => ({
    input: {
      role: "combobox",
      "aria-autocomplete": "list",
      // The listbox is always present in the palette, so the combobox is always
      // expanded. aria-activedescendant is per-highlighted-option and set by the
      // bindings (its id is not in the part-name id map), not projected here.
      "aria-expanded": "true",
      "aria-controls": ids.list || undefined,
    },
    list: {
      role: "listbox",
      "aria-label": config.label ?? "Suggestions",
    },
  }),
  keymap: (event, _state, part) => {
    if (part !== "input") return null;
    switch (event.key) {
      case "ArrowDown":
        return "highlightNext";
      case "ArrowUp":
        return "highlightPrev";
      case "Home":
        return "highlightFirst";
      case "End":
        return "highlightLast";
      case "Enter":
        return "select";
      default:
        return null;
    }
  },
};

export const command: BehaviorSpec<CommandConfig, CommandState, CommandActions, CommandPart> =
  compose("command", commandSlice);

/**
 * Per-instance projection for the many-instance `item` part. Its visibility is
 * a pure function of the query (fuzzy match against the option's value) and its
 * active look a pure function of the highlight -- so React, the WC and Astro all
 * hide and mark options identically. `hidden` is a boolean presence attr (the
 * harness asserts it via hasAttribute, which holds across all three frameworks).
 */
export function commandItemAria(
  value: string,
  state: CommandState,
  config: CommandConfig,
): AriaAttrs {
  const visible = matchesQuery(value, queryValue(state, config));
  const active = state.highlighted === value;
  return {
    "aria-selected": active ? "true" : "false",
    "data-selected": active ? "" : undefined,
    "data-highlighted": active ? "" : undefined,
    hidden: visible ? undefined : true,
  };
}

/** The ordered, enabled option values currently matching the query -- the set
 *  navigation moves over and the empty state keys off. Shared by every binding
 *  so keyboard order agrees with DOM order. */
export function visibleItemValues(root: HTMLElement, query: string): string[] {
  const values: string[] = [];
  for (const item of root.querySelectorAll<HTMLElement>('[data-part="item"]')) {
    if (item.hasAttribute("data-disabled") || item.getAttribute("aria-disabled") === "true") {
      continue;
    }
    const value = item.dataset["value"];
    if (value !== undefined && matchesQuery(value, query)) values.push(value);
  }
  return values;
}

/** The empty state shows only when the user has typed and nothing matches
 *  (ported: `visibleItems.length > 0 || !value` hides it otherwise). */
export function isEmptyShown(visibleCount: number, query: string): boolean {
  return query !== "" && visibleCount === 0;
}

/** The parts and dispatch the modal-dialog trio composes against. */
export interface CommandDialogPorts {
  /** The palette surface: focus is trapped inside it and an outside pointerdown
   *  dismisses. */
  content: HTMLElement;
  /** Resolves the trigger so the opening gesture's pointerdown is spared. */
  getTrigger: () => HTMLElement | null;
  /** Outside-pointerdown handler, already spared of the trigger. */
  onDismiss: (event: Event) => void;
}

/**
 * The modal overlay trio for the CommandDialog wrapper, composed directly the
 * way dialog.behavior.ts's startDialogModalEffects does (imitate, do not import
 * across components): trap Tab focus inside `content`, lock body scroll, and
 * dismiss on a pointerdown outside `content` sparing the trigger. Level-
 * triggered: started on the open transition, torn down (LIFO) on close/unmount.
 */
export function startCommandDialogEffects({
  content,
  getTrigger,
  onDismiss,
}: CommandDialogPorts): () => void {
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

/** The invoke event a palette raises when an option is committed (click or
 *  Enter). WC and Astro consumers listen for it; React consumers use the item's
 *  `onSelect` prop, which fires from the same click path. */
export const COMMAND_SELECT_EVENT = "command-select";

/**
 * The DOM-native binding of the command score -- the client the Web Component
 * and the Astro <script> share. Only React reads the projections declaratively.
 * The palette has no open axis (the list is always visible), so unlike bindSelect
 * there is no presence toggle and no focus roving: the input keeps focus and the
 * active option is virtual (aria-activedescendant).
 */
export function bindCommand(root: HTMLElement): () => void {
  const getPart = (part: string): HTMLElement | null =>
    part === "root" ? root : root.querySelector<HTMLElement>(`[data-part="${part}"]`);

  const inputEl = getPart("input") as HTMLInputElement | null;
  const config: CommandConfig = {
    defaultValue: inputEl?.value ?? undefined,
    label: root.dataset["label"] ?? undefined,
  };

  const { memory, dispatch } = createBehavior(command, config);

  const ids = {} as PartIds<CommandPart>;
  for (const part of Object.keys(command.parts) as CommandPart[]) {
    ids[part] = getPart(part)?.id ?? "";
  }

  const applyProjection = (el: HTMLElement, attrs: AriaAttrs) => {
    for (const [name, value] of Object.entries(attrs)) {
      updateAriaAttribute(el, name as never, value as never, { validate: false });
    }
  };

  const itemById = (value: string): HTMLElement | null =>
    root.querySelector<HTMLElement>(`[data-part="item"][data-value="${CSS.escape(value)}"]`);

  const render = () => {
    const state = memory.get();
    const query = queryValue(state, config);

    const projection = command.aria(state, config, ids);
    for (const part of Object.keys(projection) as CommandPart[]) {
      const attrs = projection[part];
      const el = getPart(part);
      if (el && attrs) applyProjection(el, attrs);
    }

    // Per-option projection: hide non-matches, mark the active one.
    let visibleCount = 0;
    for (const el of root.querySelectorAll<HTMLElement>('[data-part="item"]')) {
      const value = el.dataset["value"];
      if (value === undefined) continue;
      applyProjection(el, commandItemAria(value, state, config));
      if (!el.hidden && !el.hasAttribute("data-disabled")) visibleCount++;
    }

    // aria-activedescendant points at the highlighted option's real id.
    if (inputEl) {
      const activeEl = state.highlighted !== undefined ? itemById(state.highlighted) : null;
      if (activeEl?.id) inputEl.setAttribute("aria-activedescendant", activeEl.id);
      else inputEl.removeAttribute("aria-activedescendant");
    }

    // The empty state shows only when the query matches nothing.
    const emptyEl = getPart("empty");
    if (emptyEl) emptyEl.hidden = !isEmptyShown(visibleCount, query);
  };
  const unsubscribe = memory.subscribe(render); // fires immediately: first paint

  const onInput = () => {
    if (inputEl) dispatch("setQuery", config, inputEl.value);
  };
  inputEl?.addEventListener("input", onInput);

  const commit = (value: string) => {
    dispatch("select", config, value);
    root.dispatchEvent(new CustomEvent(COMMAND_SELECT_EVENT, { detail: { value }, bubbles: true }));
  };

  const isDisabledItem = (item: HTMLElement): boolean =>
    item.hasAttribute("data-disabled") || item.getAttribute("aria-disabled") === "true";

  const onClick = (event: Event) => {
    const item = (event.target as HTMLElement).closest<HTMLElement>('[data-part="item"]');
    if (!item || !root.contains(item) || isDisabledItem(item)) return;
    const value = item.dataset["value"];
    if (value !== undefined) commit(value);
  };
  root.addEventListener("click", onClick);

  const onPointerMove = (event: Event) => {
    const item = (event.target as HTMLElement).closest<HTMLElement>('[data-part="item"]');
    if (!item || !root.contains(item) || isDisabledItem(item)) return;
    const value = item.dataset["value"];
    if (value !== undefined && value !== memory.get().highlighted) {
      dispatch("highlight", config, value);
    }
  };
  root.addEventListener("pointermove", onPointerMove);

  const onKeydown = (event: KeyboardEvent) => {
    const partEl = (event.target as HTMLElement).closest<HTMLElement>("[data-part]");
    const part = partEl?.dataset["part"] as CommandPart | undefined;
    if (!part) return;
    const action = command.keymap(
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
    if (action === "select") {
      // Enter invokes the highlighted option by routing through the click path,
      // so keyboard and pointer commit identically (fixes the oracle's Enter,
      // which only highlighted and never fired the option's action).
      const highlighted = memory.get().highlighted;
      if (highlighted !== undefined) itemById(highlighted)?.click();
      return;
    }
    // Navigation needs the live visible-value set, which is DOM-derived.
    const visible = visibleItemValues(root, queryValue(memory.get(), config));
    dispatch(action, config, visible);
  };
  root.addEventListener("keydown", onKeydown);

  return () => {
    unsubscribe();
    inputEl?.removeEventListener("input", onInput);
    root.removeEventListener("click", onClick);
    root.removeEventListener("pointermove", onPointerMove);
    root.removeEventListener("keydown", onKeydown);
  };
}
