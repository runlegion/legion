/**
 * Op vocabulary dispatch (FR-EDITOR-003).
 *
 * applyOp dispatches on op.kind to the matching wrapper and returns its
 * OpResult unchanged -- no logic of its own beyond dispatch. applyOpSequence
 * folds applyOp over an ordered EditorOp[] (an inverse sequence), threading
 * `blocks` from each call into the next; this is how FR-EDITOR-002's undo
 * applies a stored `inverse`.
 *
 * Plain functions, no compose()/createBehavior/BehaviorSpec/slice dependency
 * -- per RULING-EDITOR-HISTORY's Editor interface contract the editor stays
 * outside the behavior-layer composer.
 */
import type { BaseBlock } from "@/lib/primitives/types";
import { applyMark, removeMark } from "./format";
import {
  applyConvert,
  applyDelete,
  applyInsert,
  applyMergeNext,
  applyMergePrev,
  applySplit,
} from "./structural";
import { insertText, removeText } from "./text";
import type { EditorOp, OpResult } from "./types";

export function applyOp(blocks: BaseBlock[], op: EditorOp): OpResult {
  switch (op.kind) {
    case "split":
      return applySplit(blocks, op);
    case "mergePrev":
      return applyMergePrev(blocks, op);
    case "mergeNext":
      return applyMergeNext(blocks, op);
    case "delete":
      return applyDelete(blocks, op);
    case "convert":
      return applyConvert(blocks, op);
    case "insert":
      return applyInsert(blocks, op);
    case "applyMark":
      return applyMark(blocks, op);
    case "removeMark":
      return removeMark(blocks, op);
    case "insertText":
      return insertText(blocks, op);
    case "removeText":
      return removeText(blocks, op);
    default: {
      const exhaustive: never = op;
      throw new Error(`applyOp: unknown op kind "${(exhaustive as EditorOp).kind}"`);
    }
  }
}

export function applyOpSequence(blocks: BaseBlock[], ops: EditorOp[]): BaseBlock[] {
  return ops.reduce((acc, op) => applyOp(acc, op).blocks, blocks);
}

export {
  applyConvert,
  applyDelete,
  applyInsert,
  applyMergeNext,
  applyMergePrev,
  applySplit,
} from "./structural";
export { applyMark, removeMark } from "./format";
export { insertText, removeText } from "./text";
export type { EditorOp, FormatOp, OpResult, StructuralOp, TextOp } from "./types";
