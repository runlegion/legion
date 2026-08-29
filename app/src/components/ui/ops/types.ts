/**
 * Editor op vocabulary -- types (FR-EDITOR-003)
 *
 * Kind-discriminated union of structural, format, and text ops over
 * BaseBlock[], plus the OpResult shape applyOp returns. No DOM, no
 * compose()/BehaviorSpec/slice dependency -- the editor stays outside the
 * behavior-layer composer per RULING-EDITOR-HISTORY's Editor interface
 * contract.
 */
import type { BaseBlock, InlineContent, InlineMark } from "@/lib/primitives/types";

export type StructuralOp =
  | { kind: "split"; blockId: string; offset: number; newBlockId: string }
  | { kind: "mergePrev"; blockId: string }
  | { kind: "mergeNext"; blockId: string }
  | { kind: "delete"; blockId: string }
  // `content` is OPTIONAL: unset on a normal forward convert; SET when this op
  // is itself an inverse restoring pre-convert content-with-marks.
  | {
      kind: "convert";
      blockId: string;
      newType: string;
      meta?: Record<string, unknown>;
      content?: InlineContent[];
    }
  | {
      kind: "insert";
      blocks: BaseBlock[];
      atBlockId: string;
      atOffset: number;
      splitBlockId?: string;
    };

export type FormatOp =
  | {
      kind: "applyMark";
      blockId: string;
      start: number;
      end: number;
      mark: InlineMark;
      href?: string;
    }
  | { kind: "removeMark"; blockId: string; start: number; end: number; mark: InlineMark };

// Canonical text op (RULING-EDITOR-HISTORY, Editor interface contract): ONE shape
// for insert and remove, `text` typed InlineContent[] end-to-end -- never a bare
// string, so marks on inserted/removed text always survive undo/redo.
export type TextOp =
  | { kind: "insertText"; blockId: string; offset: number; text: InlineContent[] }
  | { kind: "removeText"; blockId: string; offset: number; text: InlineContent[] };

export type EditorOp = StructuralOp | FormatOp | TextOp;

export interface OpResult {
  /** Updated block array (block-operations' copy-on-write convention: unchanged
   *  blocks keep referential identity). FR-EDITOR-002 stores this as its memory
   *  cell's `doc` field -- this issue does not touch the cell. */
  blocks: BaseBlock[];
  /** An ORDERED SEQUENCE of ops (per RULING-EDITOR-HISTORY's Editor interface
   *  contract) that undoes this op's effect on `blocks`. mergePrev/mergeNext
   *  need more than one op to undo: reversing a merge must both truncate the
   *  survivor's content back to its pre-merge boundary AND reinstate the
   *  absorbed block with its original type/meta/content. Applying the
   *  sequence IN ORDER via applyOp (or the applyOpSequence convenience below)
   *  on `result.blocks` SHALL return a document deep-equal to the ORIGINAL
   *  input blocks, including marks. Most ops produce a one-element sequence;
   *  mergePrev, mergeNext, and insert-with-implicit-split produce
   *  multi-element sequences. */
  inverse: EditorOp[];
}
