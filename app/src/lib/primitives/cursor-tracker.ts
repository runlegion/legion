/**
 * Cursor tracker primitive - reads and sets cursor position in a contentEditable
 *
 * Pure functions for working with cursor position relative to block elements
 * inside a contentEditable container. Each block element must have a
 * data-block-id attribute.
 *
 * SSR-safe: all functions return null/no-op when document.getSelection is unavailable.
 *
 * @registry-name cursor-tracker
 * @registry-version 0.1.0
 * @registry-status published
 * @registry-path primitives/cursor-tracker.ts
 * @registry-type registry:primitive
 *
 * @dependencies none
 */

// =============================================================================
// Types
// =============================================================================

export interface CursorPosition {
  /** Block element's data-block-id */
  blockId: string;
  /** Character offset within the block's text content */
  offset: number;
  /** Total text length of the block */
  blockLength: number;
}

// =============================================================================
// DOM helpers
// =============================================================================

/** Find the closest block element from a DOM node */
export function findBlockElement(node: Node | null): HTMLElement | null {
  if (!node) return null;
  const el = node instanceof HTMLElement ? node : node.parentElement;
  return el?.closest("[data-block-id]") as HTMLElement | null;
}

/** Get the text offset of the cursor within a block element */
function getTextOffset(blockEl: HTMLElement, anchorNode: Node, anchorOffset: number): number {
  const range = document.createRange();
  range.selectNodeContents(blockEl);
  range.setEnd(anchorNode, anchorOffset);
  return range.toString().length;
}

// =============================================================================
// Read cursor position
// =============================================================================

/**
 * Get the current cursor position relative to the block structure.
 * Returns null if no selection, or if the cursor is not inside a block.
 */
export function getCursorPosition(): CursorPosition | null {
  if (typeof window === "undefined") return null;

  const sel = document.getSelection();
  if (!sel || !sel.rangeCount) return null;

  const blockEl = findBlockElement(sel.anchorNode);
  if (!blockEl) return null;

  const blockId = blockEl.getAttribute("data-block-id");
  if (!blockId) return null;

  return {
    blockId,
    offset: getTextOffset(blockEl, sel.anchorNode as Node, sel.anchorOffset),
    blockLength: blockEl.textContent?.length ?? 0,
  };
}

/**
 * Check if the cursor is at the very start of a block (offset === 0).
 */
export function isCursorAtBlockStart(): boolean {
  const pos = getCursorPosition();
  return pos !== null && pos.offset === 0;
}

/**
 * Check if the cursor is at the very end of a block.
 */
export function isCursorAtBlockEnd(): boolean {
  const pos = getCursorPosition();
  return pos !== null && pos.offset === pos.blockLength;
}

/**
 * Check if the selection is collapsed (cursor, not range).
 */
export function isSelectionCollapsed(): boolean {
  if (typeof window === "undefined") return true;
  const sel = document.getSelection();
  return sel?.isCollapsed ?? true;
}

// =============================================================================
// Set cursor position
// =============================================================================

/**
 * Place the cursor at a specific offset within a block element.
 * If the offset exceeds the text length, places at the end.
 */
export function setCursorInBlock(container: HTMLElement, blockId: string, offset: number): void {
  if (typeof window === "undefined") return;

  const blockEl = container.querySelector(`[data-block-id="${blockId}"]`) as HTMLElement | null;
  if (!blockEl) return;

  const sel = document.getSelection();
  if (!sel) return;

  // Walk text nodes to find the right position
  const walker = document.createTreeWalker(blockEl, NodeFilter.SHOW_TEXT);
  let remaining = offset;

  while (walker.nextNode()) {
    const textNode = walker.currentNode as Text;
    if (remaining <= textNode.length) {
      const range = document.createRange();
      range.setStart(textNode, remaining);
      range.collapse(true);
      sel.removeAllRanges();
      sel.addRange(range);
      return;
    }
    remaining -= textNode.length;
  }

  // Offset beyond content: place at end
  const range = document.createRange();
  range.selectNodeContents(blockEl);
  range.collapse(false);
  sel.removeAllRanges();
  sel.addRange(range);
}

/**
 * Place the cursor at the end of a block.
 */
export function setCursorAtBlockEnd(container: HTMLElement, blockId: string): void {
  if (typeof window === "undefined") return;

  const blockEl = container.querySelector(`[data-block-id="${blockId}"]`) as HTMLElement | null;
  if (!blockEl) return;

  const length = blockEl.textContent?.length ?? 0;
  setCursorInBlock(container, blockId, length);
}

/**
 * Place the cursor at the start of a block.
 */
export function setCursorAtBlockStart(container: HTMLElement, blockId: string): void {
  setCursorInBlock(container, blockId, 0);
}
