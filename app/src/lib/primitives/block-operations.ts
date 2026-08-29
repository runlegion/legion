/**
 * Block operations primitive - pure functions for structural block mutations
 *
 * Split, merge, convert, insert, and delete operations on a block array.
 * No DOM, no side effects, no external dependencies. These are the building
 * blocks for Enter/Backspace/Delete/paste behavior in a document editor.
 *
 * @registry-name block-operations
 * @registry-version 0.1.0
 * @registry-status published
 * @registry-path primitives/block-operations.ts
 * @registry-type registry:primitive
 *
 * @dependencies none
 */
import type { BaseBlock, InlineContent } from "@/lib/primitives/types";

// =============================================================================
// Content helpers
// =============================================================================

/** Extract plain text from block content */
export function blockContentToText(content: string | InlineContent[] | undefined): string {
  if (content === undefined) return "";
  if (typeof content === "string") return content;
  return content.map((s) => s.text).join("");
}

/**
 * Split run-array content at a character offset, preserving marks/href on
 * each half (a run straddling the offset is itself split in two).
 *
 * Exported so ops/content.ts (components/editor/ops) can delegate to this
 * single copy instead of carrying its own -- see marksEqual/mergeRuns below
 * for the same rationale.
 */
export function splitInlineContent(
  content: InlineContent[],
  offset: number,
): [InlineContent[], InlineContent[]] {
  const before: InlineContent[] = [];
  const after: InlineContent[] = [];
  let pos = 0;
  for (const run of content) {
    const runLen = run.text.length;
    if (pos + runLen <= offset) {
      before.push(run);
    } else if (pos >= offset) {
      after.push(run);
    } else {
      const splitAt = offset - pos;
      before.push({ ...run, text: run.text.slice(0, splitAt) });
      after.push({ ...run, text: run.text.slice(splitAt) });
    }
    pos += runLen;
  }
  return [before, after];
}

/**
 * Split a block's content at a character offset, mark-aware: string content
 * splits as a string (unchanged behavior), InlineContent[] content splits by
 * run so marks are preserved on both halves -- never coerced through
 * blockContentToText.
 */
function splitContent(
  content: string | InlineContent[] | undefined,
  offset: number,
): [string | InlineContent[], string | InlineContent[]] {
  if (Array.isArray(content)) return splitInlineContent(content, offset);
  const text = content ?? "";
  return [text.slice(0, offset), text.slice(offset)];
}

export function inlineMarksEqual(a: InlineContent["marks"], b: InlineContent["marks"]): boolean {
  const as = [...(a ?? [])].sort();
  const bs = [...(b ?? [])].sort();
  if (as.length !== bs.length) return false;
  return as.every((m, i) => m === bs[i]);
}

/**
 * Merge adjacent runs with identical mark sets and href, so a merge point
 * that falls between two same-mark runs (e.g. splitting, then merging back)
 * reconstructs the original run boundaries instead of leaving a spurious
 * extra split.
 */
export function mergeAdjacentRuns(runs: InlineContent[]): InlineContent[] {
  const result: InlineContent[] = [];
  for (const run of runs) {
    if (run.text.length === 0) continue;
    const last = result[result.length - 1];
    if (last && inlineMarksEqual(last.marks, run.marks) && last.href === run.href) {
      result[result.length - 1] = { ...last, text: last.text + run.text };
    } else {
      result.push({ ...run });
    }
  }
  return result;
}

/**
 * Concatenate two blocks' content for a merge, mark-aware: two plain-string
 * (or undefined) sides concatenate as a string (unchanged behavior); if
 * either side carries InlineContent[], both sides are normalized to runs,
 * concatenated, and adjacent identical-mark runs are merged so marks survive
 * the merge point in canonical form.
 */
function concatContent(
  a: string | InlineContent[] | undefined,
  b: string | InlineContent[] | undefined,
): string | InlineContent[] {
  if (!Array.isArray(a) && !Array.isArray(b)) {
    return (typeof a === "string" ? a : "") + (typeof b === "string" ? b : "");
  }
  const runsA = Array.isArray(a) ? a : typeof a === "string" && a.length > 0 ? [{ text: a }] : [];
  const runsB = Array.isArray(b) ? b : typeof b === "string" && b.length > 0 ? [{ text: b }] : [];
  return mergeAdjacentRuns([...runsA, ...runsB]);
}

// =============================================================================
// Split: Enter pressed in the middle of a block
// =============================================================================

export interface SplitResult {
  /** Updated blocks array */
  blocks: BaseBlock[];
  /** ID of the new block (the second half) */
  newBlockId: string;
}

/**
 * Split a block at a text offset. The block keeps content before the offset,
 * a new text block gets content after the offset.
 *
 * If offset is 0: the current block becomes empty, new block gets all content.
 * If offset is at end: current block keeps all content, new empty block created.
 * Headings always create a text block after (not another heading).
 */
export function splitBlock(
  blocks: BaseBlock[],
  blockId: string,
  offset: number,
  newBlockId: string,
): SplitResult {
  const index = blocks.findIndex((b) => b.id === blockId);
  if (index === -1) return { blocks, newBlockId: "" };

  const block = blocks[index];
  if (!block) return { blocks, newBlockId: "" };

  const [before, after] = splitContent(block.content, offset);

  // The new block is always a text block (even after headings)
  const newBlock: BaseBlock = {
    id: newBlockId,
    type: "text",
    content: after,
  };

  const updatedBlock: BaseBlock = {
    ...block,
    content: before,
  };

  const next = [...blocks];
  next[index] = updatedBlock;
  next.splice(index + 1, 0, newBlock);

  return { blocks: next, newBlockId };
}

// =============================================================================
// Merge: Backspace at start or Delete at end
// =============================================================================

export interface MergeResult {
  /** Updated blocks array */
  blocks: BaseBlock[];
  /** ID of the surviving block */
  survivorId: string;
  /** Cursor offset in the surviving block (where the merge point is) */
  cursorOffset: number;
}

/**
 * Merge a block with the previous block. The previous block absorbs the
 * content of the current block. The current block is deleted.
 */
export function mergeWithPrevious(blocks: BaseBlock[], blockId: string): MergeResult {
  const index = blocks.findIndex((b) => b.id === blockId);
  if (index <= 0) return { blocks, survivorId: blockId, cursorOffset: 0 };

  const prevBlock = blocks[index - 1];
  const currentBlock = blocks[index];
  if (!prevBlock || !currentBlock) return { blocks, survivorId: blockId, cursorOffset: 0 };

  const cursorOffset = blockContentToText(prevBlock.content).length;

  const merged: BaseBlock = {
    ...prevBlock,
    content: concatContent(prevBlock.content, currentBlock.content),
  };

  const next = [...blocks];
  next[index - 1] = merged;
  next.splice(index, 1);

  return { blocks: next, survivorId: prevBlock.id, cursorOffset };
}

/**
 * Merge a block with the next block. The current block absorbs the content
 * of the next block. The next block is deleted.
 */
export function mergeWithNext(blocks: BaseBlock[], blockId: string): MergeResult {
  const index = blocks.findIndex((b) => b.id === blockId);
  if (index === -1 || index >= blocks.length - 1) {
    return { blocks, survivorId: blockId, cursorOffset: 0 };
  }

  const currentBlock = blocks[index];
  const nextBlock = blocks[index + 1];
  if (!currentBlock || !nextBlock) {
    return { blocks, survivorId: blockId, cursorOffset: 0 };
  }

  const cursorOffset = blockContentToText(currentBlock.content).length;

  const merged: BaseBlock = {
    ...currentBlock,
    content: concatContent(currentBlock.content, nextBlock.content),
  };

  const next = [...blocks];
  next[index] = merged;
  next.splice(index + 1, 1);

  return { blocks: next, survivorId: currentBlock.id, cursorOffset };
}

// =============================================================================
// Delete: remove a block
// =============================================================================

export interface DeleteResult {
  /** Updated blocks array */
  blocks: BaseBlock[];
  /** ID of the block to focus after deletion */
  focusBlockId: string | null;
  /** Whether to place cursor at end of the focus block */
  focusAtEnd: boolean;
}

/**
 * Delete a block from the array. Returns the block to focus after deletion
 * (the previous block, or the next, or null if the array is now empty).
 */
export function deleteBlock(blocks: BaseBlock[], blockId: string): DeleteResult {
  const index = blocks.findIndex((b) => b.id === blockId);
  if (index === -1) return { blocks, focusBlockId: null, focusAtEnd: false };

  const next = blocks.filter((b) => b.id !== blockId);

  // Focus the previous block (at end), or next block (at start), or null
  let focusBlockId: string | null = null;
  let focusAtEnd = false;
  const prevBlock = index > 0 ? next[index - 1] : undefined;
  const nextBlock = next[index];
  if (prevBlock) {
    focusBlockId = prevBlock.id;
    focusAtEnd = true;
  } else if (nextBlock) {
    focusBlockId = nextBlock.id;
    focusAtEnd = false;
  }

  return { blocks: next, focusBlockId, focusAtEnd };
}

// =============================================================================
// Convert: change block type
// =============================================================================

/**
 * Change a block's type, preserving its content. For certain conversions,
 * content is transformed (e.g., code blocks strip marks).
 */
export function convertBlockType(
  blocks: BaseBlock[],
  blockId: string,
  newType: string,
  meta?: Record<string, unknown>,
): BaseBlock[] {
  return blocks.map((b) => {
    if (b.id !== blockId) return b;

    const updated: BaseBlock = {
      ...b,
      type: newType,
    };

    if (meta) {
      updated.meta = { ...b.meta, ...meta };
    } else if (newType === "text" && b.meta) {
      // When converting to text, remove type-specific meta (level, etc.)
      const { level: _, ...rest } = b.meta as Record<string, unknown> & { level?: number };
      if (Object.keys(rest).length > 0) {
        updated.meta = rest;
      } else {
        delete (updated as unknown as Record<string, unknown>).meta;
      }
    }

    // Code blocks: flatten content to plain text
    if (newType === "code" && Array.isArray(b.content)) {
      updated.content = blockContentToText(b.content);
    }

    return updated;
  });
}

// =============================================================================
// Insert: paste blocks at a position
// =============================================================================

export interface InsertResult {
  /** Updated blocks array */
  blocks: BaseBlock[];
  /** ID of the last inserted block (for cursor placement) */
  lastInsertedId: string;
}

/**
 * Insert blocks at a position. If the cursor is in the middle of a block,
 * that block is split first and the new blocks are inserted between the halves.
 */
export function insertBlocksAt(
  blocks: BaseBlock[],
  newBlocks: BaseBlock[],
  atBlockId: string,
  atOffset: number,
  splitBlockId?: string,
): InsertResult {
  if (newBlocks.length === 0) return { blocks, lastInsertedId: atBlockId };

  const lastInserted = newBlocks[newBlocks.length - 1];
  if (!lastInserted) return { blocks, lastInsertedId: atBlockId };

  const index = blocks.findIndex((b) => b.id === atBlockId);
  if (index === -1) return { blocks, lastInsertedId: lastInserted.id };

  const block = blocks[index];
  if (!block) return { blocks, lastInsertedId: lastInserted.id };

  const text = blockContentToText(block.content);

  // If at the very end, just insert after
  if (atOffset >= text.length) {
    const next = [...blocks];
    next.splice(index + 1, 0, ...newBlocks);
    return { blocks: next, lastInsertedId: lastInserted.id };
  }

  // If at the very start, insert before
  if (atOffset === 0) {
    const next = [...blocks];
    next.splice(index, 0, ...newBlocks);
    return { blocks: next, lastInsertedId: lastInserted.id };
  }

  // In the middle: split the block and insert between halves
  if (!splitBlockId) {
    throw new Error(
      `insertBlocksAt: splitBlockId required to split block "${atBlockId}" at offset ${atOffset}`,
    );
  }
  const { blocks: splitBlocks, newBlockId } = splitBlock(blocks, atBlockId, atOffset, splitBlockId);
  const splitIndex = splitBlocks.findIndex((b) => b.id === newBlockId);
  if (splitIndex === -1) return { blocks: splitBlocks, lastInsertedId: lastInserted.id };

  // Insert new blocks before the second half
  const next = [...splitBlocks];
  next.splice(splitIndex, 0, ...newBlocks);
  return { blocks: next, lastInsertedId: lastInserted.id };
}
