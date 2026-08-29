/**
 * Structural ops (FR-EDITOR-003) -- wrap block-operations, capturing what its
 * return shape omits, and deriving each op's inverse as an EditorOp[]
 * sequence, per RULING-EDITOR-HISTORY's Editor interface contract.
 *
 * Ops carry PRE-ASSIGNED ids for any block they create; these wrappers inject
 * those ids into the primitives -- they never mint ids themselves, so redo
 * replays with the same ids the first application used.
 */
import {
  blockContentToText,
  convertBlockType,
  deleteBlock,
  insertBlocksAt,
  mergeWithNext,
  mergeWithPrevious,
  splitBlock,
} from "@/lib/primitives/block-operations";
import type { BaseBlock } from "@/lib/primitives/types";
import { mergeRuns, normalizeRuns } from "./content";
import type { EditorOp, OpResult, StructuralOp } from "./types";

export function applySplit(
  blocks: BaseBlock[],
  op: Extract<StructuralOp, { kind: "split" }>,
): OpResult {
  const { blockId, offset, newBlockId } = op;
  const block = blocks.find((b) => b.id === blockId);
  if (!block) throw new Error(`applySplit: block "${blockId}" not found`);
  const total = blockContentToText(block.content).length;
  if (offset < 0 || offset > total) {
    throw new Error(
      `applySplit: offset ${offset} out of bounds for block "${blockId}" (length ${total})`,
    );
  }

  const result = splitBlock(blocks, blockId, offset, newBlockId);

  return {
    blocks: result.blocks,
    inverse: [{ kind: "mergeNext", blockId }],
  };
}

export function applyMergePrev(
  blocks: BaseBlock[],
  op: Extract<StructuralOp, { kind: "mergePrev" }>,
): OpResult {
  const { blockId } = op;
  const index = blocks.findIndex((b) => b.id === blockId);
  if (index === -1) throw new Error(`applyMergePrev: block "${blockId}" not found`);
  if (index <= 0) throw new Error(`applyMergePrev: block "${blockId}" has no previous block`);

  // Capture the absorbed block verbatim before merging -- it is reinstated
  // exactly as-is by the inverse's insert.
  const absorbed = blocks[index] as BaseBlock;
  const result = mergeWithPrevious(blocks, blockId);

  const inverse: EditorOp[] = [
    {
      kind: "removeText",
      blockId: result.survivorId,
      offset: result.cursorOffset,
      // Canonicalized (mergeRuns), not the raw captured content: the
      // survivor's post-merge content went through concatContent's own
      // mergeAdjacentRuns pass, so adjacent same-mark runs in `absorbed`
      // (a normal shape for pasted/deserialized content) are already
      // coalesced there. removeText does an exact runsEqual check against
      // this `text`, so it must match that canonical form or it throws
      // instead of undoing. The reinstated block below still gets the RAW
      // `absorbed` -- only this equality-check copy is canonicalized.
      text: mergeRuns(normalizeRuns(absorbed.content)),
    },
    {
      kind: "insert",
      blocks: [absorbed],
      atBlockId: result.survivorId,
      atOffset: result.cursorOffset,
    },
  ];

  return { blocks: result.blocks, inverse };
}

export function applyMergeNext(
  blocks: BaseBlock[],
  op: Extract<StructuralOp, { kind: "mergeNext" }>,
): OpResult {
  const { blockId } = op;
  const index = blocks.findIndex((b) => b.id === blockId);
  if (index === -1) throw new Error(`applyMergeNext: block "${blockId}" not found`);
  if (index >= blocks.length - 1)
    throw new Error(`applyMergeNext: block "${blockId}" has no next block`);

  const absorbed = blocks[index + 1] as BaseBlock;
  const result = mergeWithNext(blocks, blockId);

  const inverse: EditorOp[] = [
    {
      kind: "removeText",
      blockId: result.survivorId,
      offset: result.cursorOffset,
      // Canonicalized (mergeRuns), not the raw captured content: the
      // survivor's post-merge content went through concatContent's own
      // mergeAdjacentRuns pass, so adjacent same-mark runs in `absorbed`
      // (a normal shape for pasted/deserialized content) are already
      // coalesced there. removeText does an exact runsEqual check against
      // this `text`, so it must match that canonical form or it throws
      // instead of undoing. The reinstated block below still gets the RAW
      // `absorbed` -- only this equality-check copy is canonicalized.
      text: mergeRuns(normalizeRuns(absorbed.content)),
    },
    {
      kind: "insert",
      blocks: [absorbed],
      atBlockId: result.survivorId,
      atOffset: result.cursorOffset,
    },
  ];

  return { blocks: result.blocks, inverse };
}

export function applyDelete(
  blocks: BaseBlock[],
  op: Extract<StructuralOp, { kind: "delete" }>,
): OpResult {
  const { blockId } = op;
  const index = blocks.findIndex((b) => b.id === blockId);
  if (index === -1) throw new Error(`applyDelete: block "${blockId}" not found`);

  const captured = blocks[index] as BaseBlock;
  const prev = index > 0 ? blocks[index - 1] : undefined;
  const next = index < blocks.length - 1 ? blocks[index + 1] : undefined;
  if (!prev && !next) {
    throw new Error(
      `applyDelete: block "${blockId}" is the only block -- no anchor for its inverse insert`,
    );
  }

  const result = deleteBlock(blocks, blockId);

  // Boundary insert at the captured index (no splitBlockId needed): anchor on
  // the previous block's end when one exists, else the next block's start.
  const insertOp: StructuralOp = prev
    ? {
        kind: "insert",
        blocks: [captured],
        atBlockId: prev.id,
        atOffset: blockContentToText(prev.content).length,
      }
    : { kind: "insert", blocks: [captured], atBlockId: (next as BaseBlock).id, atOffset: 0 };

  return { blocks: result.blocks, inverse: [insertOp] };
}

export function applyConvert(
  blocks: BaseBlock[],
  op: Extract<StructuralOp, { kind: "convert" }>,
): OpResult {
  const { blockId, newType, meta, content } = op;
  const block = blocks.find((b) => b.id === blockId);
  if (!block) throw new Error(`applyConvert: block "${blockId}" not found`);

  const priorType = block.type;
  const priorMeta = block.meta;
  const priorContent = block.content;

  let nextBlocks = convertBlockType(blocks, blockId, newType, meta);
  if (content !== undefined) {
    // Bypass convertBlockType's own (possibly lossy) content transform --
    // this op is itself restoring a captured pre-convert snapshot.
    nextBlocks = nextBlocks.map((b) => (b.id === blockId ? { ...b, content } : b));
  }

  const inverseOp: StructuralOp = {
    kind: "convert",
    blockId,
    newType: priorType,
    ...(priorMeta !== undefined ? { meta: priorMeta } : {}),
    // Only restore content explicitly when the prior content carried marks --
    // converting to 'code' is the one lossy transform (flattens InlineContent[]
    // to plain text) that needs undoing this way. Plain string content already
    // survives non-'code' conversions unchanged, so leaving this unset here
    // keeps a plain-string block's content plain-string after undo.
    ...(Array.isArray(priorContent) ? { content: priorContent } : {}),
  };

  return { blocks: nextBlocks, inverse: [inverseOp] };
}

export function applyInsert(
  blocks: BaseBlock[],
  op: Extract<StructuralOp, { kind: "insert" }>,
): OpResult {
  const { blocks: toInsert, atBlockId, atOffset, splitBlockId } = op;
  const targetIndex = blocks.findIndex((b) => b.id === atBlockId);
  if (targetIndex === -1) throw new Error(`applyInsert: block "${atBlockId}" not found`);

  const targetBlock = blocks[targetIndex] as BaseBlock;
  const total = blockContentToText(targetBlock.content).length;
  if (atOffset < 0 || atOffset > total) {
    throw new Error(
      `applyInsert: offset ${atOffset} out of bounds for block "${atBlockId}" (length ${total})`,
    );
  }
  // insertBlocksAt no-ops (no split, no insert) when there is nothing to
  // insert, even for a mid-block offset -- the inverse must match that.
  const willSplit = toInsert.length > 0 && atOffset > 0 && atOffset < total;
  if (willSplit && !splitBlockId) {
    throw new Error(
      `applyInsert: splitBlockId required to insert into the middle of block "${atBlockId}"`,
    );
  }

  const result = insertBlocksAt(blocks, toInsert, atBlockId, atOffset, splitBlockId);

  // One delete per inserted block (same order), plus -- only when a split
  // occurred -- a trailing mergeNext reuniting the two split halves.
  const deletes: EditorOp[] = toInsert.map((b) => ({ kind: "delete", blockId: b.id }));
  const inverse: EditorOp[] = willSplit
    ? [...deletes, { kind: "mergeNext", blockId: atBlockId }]
    : deletes;

  return { blocks: result.blocks, inverse };
}
