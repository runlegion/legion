/**
 * Text ops (FR-EDITOR-003) -- intra-block insert/remove over InlineContent[].
 *
 * No text primitive exists in block-operations.ts -- this is new code, not a
 * reimplementation of anything.
 */
import type { BaseBlock, InlineContent } from "@/lib/primitives/types";
import {
  collapseIfPlain,
  mergeRuns,
  normalizeRuns,
  runsEqual,
  splitRuns,
  totalTextLength,
} from "./content";
import type { OpResult, TextOp } from "./types";

function findBlockIndex(blocks: BaseBlock[], blockId: string, opName: string): number {
  const index = blocks.findIndex((b) => b.id === blockId);
  if (index === -1) throw new Error(`${opName}: block "${blockId}" not found`);
  return index;
}

export function insertText(
  blocks: BaseBlock[],
  op: Extract<TextOp, { kind: "insertText" }>,
): OpResult {
  const { blockId, offset, text } = op;
  const index = findBlockIndex(blocks, blockId, "insertText");
  const block = blocks[index] as BaseBlock;
  const runs = normalizeRuns(block.content);
  const total = totalTextLength(runs);
  if (offset < 0 || offset > total) {
    throw new Error(
      `insertText: offset ${offset} out of bounds for block "${blockId}" (length ${total})`,
    );
  }

  const [before, after] = splitRuns(runs, offset);
  const content = collapseIfPlain(mergeRuns([...before, ...text, ...after]));

  const newBlocks = [...blocks];
  newBlocks[index] = { ...block, content };

  return {
    blocks: newBlocks,
    inverse: [{ kind: "removeText", blockId, offset, text }],
  };
}

export function removeText(
  blocks: BaseBlock[],
  op: Extract<TextOp, { kind: "removeText" }>,
): OpResult {
  const { blockId, offset, text } = op;
  const index = findBlockIndex(blocks, blockId, "removeText");
  const block = blocks[index] as BaseBlock;
  const runs = normalizeRuns(block.content);
  const total = totalTextLength(runs);
  const removedLength = totalTextLength(text);
  if (offset < 0 || offset + removedLength > total) {
    throw new Error(
      `removeText: range [${offset}, ${offset + removedLength}) out of bounds for block "${blockId}" (length ${total})`,
    );
  }

  const [before, fromOffset] = splitRuns(runs, offset);
  const [middle, after] = splitRuns(fromOffset, removedLength);

  const actual: InlineContent[] = middle;
  if (!runsEqual(actual, text)) {
    throw new Error(
      `removeText: content at [${offset}, ${offset + removedLength}) in block "${blockId}" does not match expected text`,
    );
  }

  const content = collapseIfPlain(mergeRuns([...before, ...after]));

  const newBlocks = [...blocks];
  newBlocks[index] = { ...block, content };

  return {
    blocks: newBlocks,
    inverse: [{ kind: "insertText", blockId, offset, text }],
  };
}
