/**
 * Block canvas primitive - selection, focus, and keyboard management for block editors
 *
 * Manages the core state machine for a block-based editing surface:
 * - Selection (single, multi via Cmd/Ctrl, range via Shift)
 * - Focus traversal (arrow keys, Home/End)
 * - Keyboard shortcuts (Escape to clear, Space/Enter to toggle, Cmd+A to select all)
 * - Slash command trigger
 *
 * @registry-name block-canvas
 * @registry-version 0.1.0
 * @registry-status published
 * @registry-path primitives/block-canvas.ts
 * @registry-type registry:primitive
 *
 * @cognitive-load 5/10 - Complex state machine but familiar canvas/listbox pattern
 * @attention-economics Primary editing surface; selection indicators and focus rings guide attention
 * @trust-building Clear selection feedback (single/multi/range), keyboard navigation hints via ARIA
 * @accessibility ARIA listbox with multiselectable, activedescendant focus, live region for selection announcements
 * @semantic-meaning Canvas = block container; selection and focus are separate concepts (focus = keyboard cursor, selection = acted-upon set)
 *
 * @usage-patterns
 * DO: Attach to a container element with role="listbox" and tabIndex=0
 * DO: Use getBlocks/getSelectedIds/getFocusedId callbacks for current state
 * DO: Call destroy() on cleanup to remove all keyboard listeners
 * NEVER: Mutate blocks array directly -- use onSelectionChange/onFocusChange callbacks
 * NEVER: Mix focus and selection concepts in the consuming UI
 *
 * @example
 * ```ts
 * const canvas = createBlockCanvas({
 *   container: document.getElementById('editor'),
 *   getBlocks: () => blocks,
 *   getSelectedIds: () => selectedIds,
 *   getFocusedId: () => focusedId,
 *   onSelectionChange: (ids) => setSelection(ids),
 *   onFocusChange: (id) => setFocused(id),
 * });
 *
 * canvas.handleBlockClick('block-1', { meta: false, shift: false });
 * canvas.destroy();
 * ```
 */
import { createKeyboardHandler } from "@/lib/primitives/keyboard-handler";
import type { CleanupFunction } from "@/lib/primitives/types";

// ============================================================================
// Types
// ============================================================================

export interface BlockCanvasBlock {
  id: string;
  type: string;
}

export interface BlockCanvasOptions {
  /** Container element for keyboard event listeners */
  container: HTMLElement;
  /** Get current blocks array */
  getBlocks: () => BlockCanvasBlock[];
  /** Get current selected IDs */
  getSelectedIds: () => Set<string>;
  /** Get current focused block ID */
  getFocusedId: () => string | undefined;
  /** Called when selection changes */
  onSelectionChange: (ids: Set<string>) => void;
  /** Called when focus changes */
  onFocusChange?: (id: string | null) => void;
  /** Called when slash command is triggered */
  onSlashCommand?: (position: { x: number; y: number }) => void;
}

export interface BlockClickOptions {
  meta: boolean;
  shift: boolean;
}

export interface BlockCanvasControls {
  /** Handle a click on a specific block */
  handleBlockClick: (blockId: string, options: BlockClickOptions) => void;
  /** Handle a click on the canvas background (not on a block) */
  handleCanvasBackgroundClick: () => void;
  /** Select all blocks */
  selectAll: () => void;
  /** Clear selection */
  clearSelection: () => void;
  /** Move focus in a direction */
  moveFocus: (direction: "up" | "down" | "first" | "last") => void;
  /** Clean up event listeners */
  destroy: CleanupFunction;
}

// ============================================================================
// Implementation
// ============================================================================

function isFromEditable(event: KeyboardEvent): boolean {
  const target = event.target as HTMLElement;
  if (!target) return false;
  if (target.isContentEditable) return true;
  const tagName = target.tagName.toLowerCase();
  return tagName === "input" || tagName === "textarea" || tagName === "select";
}

function guardHandler(handler: () => void) {
  return (event: KeyboardEvent): void => {
    if (isFromEditable(event)) return;
    event.preventDefault();
    handler();
  };
}

export function createBlockCanvas(options: BlockCanvasOptions): BlockCanvasControls {
  const {
    container,
    getBlocks,
    getSelectedIds,
    getFocusedId,
    onSelectionChange,
    onFocusChange,
    onSlashCommand,
  } = options;

  const cleanups: CleanupFunction[] = [];

  // ------------------------------------------------------------------
  // Index helpers
  // ------------------------------------------------------------------
  function getFocusedIndex(): number {
    const focusedId = getFocusedId();
    if (!focusedId) return -1;
    return getBlocks().findIndex((b) => b.id === focusedId);
  }

  function getBlockIndexMap(): Map<string, number> {
    const map = new Map<string, number>();
    const blocks = getBlocks();
    for (let i = 0; i < blocks.length; i += 1) {
      const block = blocks[i];
      if (block) map.set(block.id, i);
    }
    return map;
  }

  // ------------------------------------------------------------------
  // Focus
  // ------------------------------------------------------------------
  function moveFocus(direction: "up" | "down" | "first" | "last"): void {
    const blocks = getBlocks();
    if (blocks.length === 0) return;

    const focusedIndex = getFocusedIndex();
    let newIndex: number;

    switch (direction) {
      case "up":
        newIndex = focusedIndex <= 0 ? 0 : focusedIndex - 1;
        break;
      case "down":
        newIndex = focusedIndex >= blocks.length - 1 ? blocks.length - 1 : focusedIndex + 1;
        break;
      case "first":
        newIndex = 0;
        break;
      case "last":
        newIndex = blocks.length - 1;
        break;
    }

    const newBlock = blocks[newIndex];
    if (newBlock) onFocusChange?.(newBlock.id);
  }

  // ------------------------------------------------------------------
  // Selection
  // ------------------------------------------------------------------
  function selectAll(): void {
    const allIds = new Set(getBlocks().map((b) => b.id));
    onSelectionChange(allIds);
  }

  function clearSelection(): void {
    onSelectionChange(new Set());
  }

  function toggleSelectionOnFocused(): void {
    const focusedId = getFocusedId();
    if (!focusedId) return;
    const selectedIds = getSelectedIds();
    const newSelection = new Set(selectedIds);
    if (newSelection.has(focusedId)) {
      newSelection.delete(focusedId);
    } else {
      newSelection.add(focusedId);
    }
    onSelectionChange(newSelection);
  }

  function extendSelection(direction: "up" | "down"): void {
    const blocks = getBlocks();
    const focusedIndex = getFocusedIndex();
    if (blocks.length === 0 || focusedIndex === -1) return;

    const newIndex =
      direction === "up"
        ? Math.max(0, focusedIndex - 1)
        : Math.min(blocks.length - 1, focusedIndex + 1);

    const newBlock = blocks[newIndex];
    if (!newBlock) return;

    const selectedIds = getSelectedIds();
    const newSelection = new Set(selectedIds);
    newSelection.add(newBlock.id);
    onSelectionChange(newSelection);
    onFocusChange?.(newBlock.id);
  }

  // ------------------------------------------------------------------
  // Click handling
  // ------------------------------------------------------------------
  function handleBlockClick(blockId: string, opts: BlockClickOptions): void {
    onFocusChange?.(blockId);
    const blocks = getBlocks();
    const selectedIds = getSelectedIds();
    const focusedId = getFocusedId();
    const blockIndexMap = getBlockIndexMap();

    if (opts.shift && focusedId) {
      const fromIndex = blockIndexMap.get(focusedId) ?? 0;
      const toIndex = blockIndexMap.get(blockId) ?? 0;
      const start = Math.min(fromIndex, toIndex);
      const end = Math.max(fromIndex, toIndex);
      const rangeIds = new Set<string>();
      for (let i = start; i <= end; i++) {
        const block = blocks[i];
        if (block) rangeIds.add(block.id);
      }
      onSelectionChange(rangeIds);
    } else if (opts.meta) {
      const newSelection = new Set(selectedIds);
      if (newSelection.has(blockId)) {
        newSelection.delete(blockId);
      } else {
        newSelection.add(blockId);
      }
      onSelectionChange(newSelection);
    } else {
      onSelectionChange(new Set([blockId]));
    }
  }

  function handleCanvasBackgroundClick(): void {
    clearSelection();
    onFocusChange?.(null);
  }

  // ------------------------------------------------------------------
  // Keyboard handlers
  // ------------------------------------------------------------------
  cleanups.push(
    createKeyboardHandler(container, {
      key: "ArrowUp",
      handler: guardHandler(() => moveFocus("up")),
      preventDefault: false,
    }),
    createKeyboardHandler(container, {
      key: "ArrowDown",
      handler: guardHandler(() => moveFocus("down")),
      preventDefault: false,
    }),
    createKeyboardHandler(container, {
      key: "Home",
      handler: guardHandler(() => moveFocus("first")),
      preventDefault: false,
    }),
    createKeyboardHandler(container, {
      key: "End",
      handler: guardHandler(() => moveFocus("last")),
      preventDefault: false,
    }),
    createKeyboardHandler(container, {
      key: "ArrowUp",
      modifiers: { shift: true },
      handler: guardHandler(() => extendSelection("up")),
      preventDefault: false,
    }),
    createKeyboardHandler(container, {
      key: "ArrowDown",
      modifiers: { shift: true },
      handler: guardHandler(() => extendSelection("down")),
      preventDefault: false,
    }),
    createKeyboardHandler(container, {
      key: "Escape",
      handler: guardHandler(clearSelection),
      preventDefault: false,
    }),
    createKeyboardHandler(container, {
      key: "Space",
      handler: guardHandler(toggleSelectionOnFocused),
      preventDefault: false,
    }),
    createKeyboardHandler(container, {
      key: "Enter",
      handler: guardHandler(toggleSelectionOnFocused),
      preventDefault: false,
    }),
  );

  // Cmd/Ctrl+A for select all
  const handleSelectAll = (event: KeyboardEvent) => {
    if (isFromEditable(event)) return;
    if (event.key === "a" && (event.metaKey || event.ctrlKey)) {
      event.preventDefault();
      selectAll();
    }
  };
  container.addEventListener("keydown", handleSelectAll);
  cleanups.push(() => container.removeEventListener("keydown", handleSelectAll));

  // Slash command trigger
  if (onSlashCommand) {
    const handleSlashKey = (event: KeyboardEvent) => {
      if (isFromEditable(event)) return;
      if (event.key === "/") {
        event.preventDefault();
        const rect = container.getBoundingClientRect();
        onSlashCommand({
          x: rect.left + rect.width / 2 - 144,
          y: rect.top + 100,
        });
      }
    };
    container.addEventListener("keydown", handleSlashKey);
    cleanups.push(() => container.removeEventListener("keydown", handleSlashKey));
  }

  // ------------------------------------------------------------------
  // Public API
  // ------------------------------------------------------------------
  return {
    handleBlockClick,
    handleCanvasBackgroundClick,
    selectAll,
    clearSelection,
    moveFocus,
    destroy() {
      for (const cleanup of cleanups) cleanup();
    },
  };
}
