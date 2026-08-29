import { createMemory, type Memory } from "@/lib/primitives/memory";

export interface KeyInput {
  key: string;
  shiftKey?: boolean;
  ctrlKey?: boolean;
  altKey?: boolean;
  metaKey?: boolean;
}

export type AriaValue = string | boolean | undefined;

/**
 * The WAI-ARIA role vocabulary, verbatim from the host JSX typings (Astro's
 * `AriaRole`, a subset of React's). A projected `role` is not a free string:
 * it lands on `role=` of a real element, and the element's typing accepts only
 * this union. Declaring it here means the projection is already the type the
 * DOM wants, so no performance has to cast on the way out.
 */
export type AriaRole =
  | "alert"
  | "alertdialog"
  | "application"
  | "article"
  | "banner"
  | "blockquote"
  | "button"
  | "caption"
  | "cell"
  | "checkbox"
  | "code"
  | "columnheader"
  | "combobox"
  | "command"
  | "complementary"
  | "composite"
  | "contentinfo"
  | "definition"
  | "deletion"
  | "dialog"
  | "directory"
  | "document"
  | "emphasis"
  | "feed"
  | "figure"
  | "form"
  | "generic"
  | "grid"
  | "gridcell"
  | "group"
  | "heading"
  | "img"
  | "input"
  | "insertion"
  | "landmark"
  | "link"
  | "list"
  | "listbox"
  | "listitem"
  | "log"
  | "main"
  | "marquee"
  | "math"
  | "meter"
  | "menu"
  | "menubar"
  | "menuitem"
  | "menuitemcheckbox"
  | "menuitemradio"
  | "navigation"
  | "none"
  | "note"
  | "option"
  | "paragraph"
  | "presentation"
  | "progressbar"
  | "radio"
  | "radiogroup"
  | "range"
  | "region"
  | "roletype"
  | "row"
  | "rowgroup"
  | "rowheader"
  | "scrollbar"
  | "search"
  | "searchbox"
  | "section"
  | "sectionhead"
  | "select"
  | "separator"
  | "slider"
  | "spinbutton"
  | "status"
  | "strong"
  | "structure"
  | "subscript"
  | "superscript"
  | "switch"
  | "tab"
  | "table"
  | "tablist"
  | "tabpanel"
  | "term"
  | "textbox"
  | "time"
  | "timer"
  | "toolbar"
  | "tooltip"
  | "tree"
  | "treegrid"
  | "treeitem"
  | "widget"
  | "window";

export interface AriaAttrs {
  /** Narrower than the index signature on purpose -- see `AriaRole`. */
  role?: AriaRole | undefined;
  [attr: string]: AriaValue;
}

export interface PartDecl {
  role?: string;
  many?: boolean;
  optional?: boolean;
}

export type PartIds<Part extends string> = Record<Part, string>;
export type ActionPayloads = Record<string, unknown>;
export type PayloadArgs<P> = P extends undefined ? [] : [payload: P];

/**
 * The ids of a single many-part INSTANCE, keyed by part name -- the sibling
 * ids `instanceAria` wires against (e.g. a trigger reads its content's id and
 * vice versa). Partial because `many` is a runtime PartDecl field, not narrowed
 * at the type level: only the instance's own many parts carry a key.
 */
export type InstanceIds<Part extends string> = Partial<Record<Part, string>>;

export interface BehaviorSpec<Config, State, Actions extends ActionPayloads, Part extends string> {
  name: string;
  parts: Record<Part, PartDecl>;
  initialState: (config: Config) => State;

  actions: {
    [K in keyof Actions]: (state: State, payload: Actions[K]) => State;
  };

  canDispatch: (state: State, action: keyof Actions, config: Config) => boolean;

  aria: (state: State, config: Config, ids: PartIds<Part>) => Partial<Record<Part, AriaAttrs>>;

  /**
   * Per-instance ARIA for `many` parts. `aria()` projects one AriaAttrs per
   * part NAME; a `many` part occurs once per instance value (trigger/content
   * per menu item), which one AriaAttrs cannot express. `instanceAria` projects
   * the attrs for ONE instance of ONE part, given its value and its sibling
   * ids. OPTIONAL: statics and uniform-item components (radio-group's N-uniform
   * roving needs no per-instance ARIA -- #1870) omit it, so it never cascades
   * to the ~30 static specs.
   */
  instanceAria?: (
    part: Part,
    value: string,
    state: State,
    config: Config,
    ids: InstanceIds<Part>,
  ) => AriaAttrs;

  keymap: (event: KeyInput, state: State, part: Part, config: Config) => keyof Actions | null;
}

export function createBehavior<Config, State, Actions extends ActionPayloads, Part extends string>(
  spec: BehaviorSpec<Config, State, Actions, Part>,
  initialConfig: Config,
): {
  memory: Memory<State>;
  dispatch: <K extends keyof Actions>(
    action: K,
    config: Config,
    ...payload: PayloadArgs<Actions[K]>
  ) => boolean;
} {
  const memory = createMemory<State>(() => spec.initialState(initialConfig));

  return {
    memory,
    // Config is a parameter, not a capture: suppression must read the
    // caller's CURRENT config, and the instance outlives any one render.
    dispatch(action, config, ...payload) {
      const state = memory.get();
      if (!spec.canDispatch(state, action, config)) return false;
      memory.set(spec.actions[action](state, payload[0] as Actions[typeof action]));
      return true;
    },
  };
}
