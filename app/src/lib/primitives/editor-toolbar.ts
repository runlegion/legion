/**
 * Editor toolbar primitive - toolbar button configuration and platform detection
 *
 * Provides toolbar button definitions, platform-aware keyboard shortcut labels,
 * and history state queries for building editor toolbars.
 *
 * @registry-name editor-toolbar
 * @registry-version 0.1.0
 * @registry-status published
 * @registry-path primitives/editor-toolbar.ts
 * @registry-type registry:primitive
 *
 * @cognitive-load 4/10 - Familiar toolbar pattern with clear action icons and shortcuts
 * @attention-economics Primary actions (undo/redo) first, formatting grouped logically; disabled states reduce noise
 * @trust-building Disabled states clearly indicate unavailable actions, tooltips explain functionality and shortcuts
 * @accessibility Full keyboard support, aria-labels for screen readers, platform-aware shortcut display
 * @semantic-meaning Undo/redo for history navigation, formatting buttons for text styling marks
 *
 * @usage-patterns
 * DO: Use getButtons() to build the toolbar UI from button definitions
 * DO: Check hasFormattingButtons before rendering separator/formatting group
 * DO: Display shortcut labels from each button's shortcut field
 * NEVER: Hard-code modifier keys -- use modifierKey from the controls
 *
 * @internal-dependencies primitives/history (via consumer-provided getHistory callback)
 *
 * @example
 * ```ts
 * const toolbar = createEditorToolbar({
 *   getHistory: () => ({ canUndo: true, canRedo: false }),
 *   onUndo: () => history.undo(),
 *   onRedo: () => history.redo(),
 *   onBold: () => applyFormat('bold'),
 * });
 *
 * const buttons = toolbar.getButtons();
 * // [{ id: 'undo', label: 'Undo', shortcut: 'Cmd+Z', disabled: false, ... }, ...]
 * ```
 */

// ============================================================================
// Types
// ============================================================================

export interface HistoryState {
  canUndo: boolean;
  canRedo: boolean;
}

export interface EditorToolbarOptions {
  /** Get current history state */
  getHistory: () => HistoryState;
  /** Undo action */
  onUndo: () => void;
  /** Redo action */
  onRedo: () => void;
  /** Formatting actions (all optional) */
  onBold?: () => void;
  onItalic?: () => void;
  onUnderline?: () => void;
  onStrikethrough?: () => void;
  onLink?: () => void;
  onCode?: () => void;
}

export type ToolbarButtonGroup = "history" | "formatting";

export interface ToolbarButton {
  id: string;
  label: string;
  shortcut: string;
  disabled: boolean;
  group: ToolbarButtonGroup;
  action: () => void;
}

export interface EditorToolbarControls {
  /** Platform-aware modifier key label */
  modifierKey: string;
  /** Get all toolbar button definitions for rendering */
  getButtons: () => ToolbarButton[];
  /** Whether any formatting buttons are configured */
  hasFormattingButtons: boolean;
}

// ============================================================================
// Platform detection
// ============================================================================

function detectMac(): boolean {
  if (typeof navigator === "undefined") return false;
  return navigator.platform?.toLowerCase().includes("mac") ?? false;
}

// ============================================================================
// Implementation
// ============================================================================

export function createEditorToolbar(options: EditorToolbarOptions): EditorToolbarControls {
  const {
    getHistory,
    onUndo,
    onRedo,
    onBold,
    onItalic,
    onUnderline,
    onStrikethrough,
    onLink,
    onCode,
  } = options;

  const mod = detectMac() ? "Cmd" : "Ctrl";

  const hasFormattingButtons =
    onBold !== undefined ||
    onItalic !== undefined ||
    onUnderline !== undefined ||
    onStrikethrough !== undefined ||
    onLink !== undefined ||
    onCode !== undefined;

  function getButtons(): ToolbarButton[] {
    const history = getHistory();
    const buttons: ToolbarButton[] = [
      {
        id: "undo",
        label: "Undo",
        shortcut: `${mod}+Z`,
        disabled: !history.canUndo,
        group: "history",
        action: onUndo,
      },
      {
        id: "redo",
        label: "Redo",
        shortcut: `${mod}+Shift+Z`,
        disabled: !history.canRedo,
        group: "history",
        action: onRedo,
      },
    ];

    if (onBold)
      buttons.push({
        id: "bold",
        label: "Bold",
        shortcut: `${mod}+B`,
        disabled: false,
        group: "formatting",
        action: onBold,
      });
    if (onItalic)
      buttons.push({
        id: "italic",
        label: "Italic",
        shortcut: `${mod}+I`,
        disabled: false,
        group: "formatting",
        action: onItalic,
      });
    if (onUnderline)
      buttons.push({
        id: "underline",
        label: "Underline",
        shortcut: `${mod}+U`,
        disabled: false,
        group: "formatting",
        action: onUnderline,
      });
    if (onStrikethrough)
      buttons.push({
        id: "strikethrough",
        label: "Strikethrough",
        shortcut: `${mod}+Shift+S`,
        disabled: false,
        group: "formatting",
        action: onStrikethrough,
      });
    if (onLink)
      buttons.push({
        id: "link",
        label: "Insert link",
        shortcut: `${mod}+K`,
        disabled: false,
        group: "formatting",
        action: onLink,
      });
    if (onCode)
      buttons.push({
        id: "code",
        label: "Code",
        shortcut: `${mod}+E`,
        disabled: false,
        group: "formatting",
        action: onCode,
      });

    return buttons;
  }

  return {
    modifierKey: mod,
    getButtons,
    hasFormattingButtons,
  };
}
