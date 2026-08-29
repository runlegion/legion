/**
 * editor-history.ts -- op-based undo/redo on one memory cell (FR-EDITOR-002).
 *
 * The editor's document state -- { doc, sel, done, undone } -- lives in a
 * SINGLE createMemory cell (RULING-EDITOR-HISTORY's Editor interface
 * contract). This is an own client, not a Slice: createEditorHistory(...)
 * returns { memory, canUndo, canRedo, controls }. No dispatch(), no
 * compose()/BehaviorSpec -- the editor stays outside the behavior-layer
 * composer per frozen Spec 00.
 *
 * This module calls FR-EDITOR-003's applyOp/applyOpSequence; it does not
 * reimplement block-structural or mark logic, and does not define a
 * parallel op shape.
 */
import { createMemory, type Memory } from "@/lib/primitives/memory";
import type { BaseBlock, InlineContent } from "@/lib/primitives/types";
// Canonical op vocabulary -- FR-EDITOR-003 / #2105. Do NOT redefine EditorOp,
// applyOp, or OpResult here; import them. OpResult.inverse is an ORDERED
// EditorOp[] SEQUENCE (AMENDMENT 2, RULING-EDITOR-HISTORY) -- mergePrev,
// mergeNext, and insert-with-implicit-split produce multi-element sequences;
// applyOpSequence folds a sequence over `blocks` in order.
import { applyOp, applyOpSequence, type EditorOp, type OpResult } from "./ops";

/** Two-axis position (FR-EDITOR-002): block-id is the block axis, offset is
 *  the inline-text axis, together one position space. */
export interface EditorPosition {
  blockId: string;
  offset: number;
}

/** Collapsed when anchor and focus are equal. */
export interface EditorSelection {
  anchor: EditorPosition;
  focus: EditorPosition;
}

/** One committed undo step: a group of ops applied atomically, each paired
 *  with its derived inverse (from #2105's OpResult), plus the selection
 *  carried before/after (FR-EDITOR-002: restored on undo/redo, never
 *  re-found).
 *
 *  `inverses` is the FLATTENED inverse sequence for the whole group: for
 *  each op in `ops`, taken in REVERSE order, that op's own OpResult.inverse
 *  sequence (already in the order applyOp expects) is appended. Applying
 *  `inverses` in array order via applyOpSequence undoes the entire group;
 *  applying `ops` in array order via applyOpSequence redoes it. For simple
 *  single-inverse ops (text/format/split/convert/delete) `inverses.length
 *  === ops.length`; a group containing a mergePrev/mergeNext/implicit-split
 *  insert is longer, since that op's own inverse is itself multi-element. */
export interface HistoryEntry {
  readonly ops: readonly EditorOp[];
  readonly inverses: readonly EditorOp[];
  readonly selBefore: EditorSelection;
  readonly selAfter: EditorSelection;
}

/** The single cell's shape per FR-EDITOR-002 and the pinned contract:
 *  ALL FOUR fields -- doc, sel, done, undone -- live in the ONE
 *  `createMemory` cell. No closure-held op-log alongside it. */
export interface EditorHistoryState {
  doc: BaseBlock[];
  sel: EditorSelection;
  done: readonly HistoryEntry[]; // op-log; most recent last
  undone: readonly HistoryEntry[];
}

export interface EditorHistoryConfig {
  /** NFR-EDITOR-002: max retained undo entries. Oldest dropped past the cap. */
  cap?: number; // default 100
  /** NFR-EDITOR-003: coalescing window for consecutive same-kind edits. */
  coalesceWindowMs?: number; // default 500
}

export interface EditorHistoryControls {
  /** Constructs and applies one EditorOp via #2105's applyOp, committing
   *  (or coalescing into) one HistoryEntry. This is the general intent
   *  entry point -- callers (a later DOM-binding issue) build the EditorOp
   *  for "user typed X at this caret" etc. and hand it here.
   *
   *  When the current selection is non-collapsed and `op` is an
   *  `insertText` targeting the selected block, the selected range is
   *  removed first (one synthesized `removeText`) so typing into a
   *  selection replaces it, per the caret-notation scenario "typing into a
   *  selection replaces it" -- both ops land in the SAME HistoryEntry, so
   *  one undo restores the pre-edit text and the pre-edit selection
   *  together. */
  apply(op: EditorOp): void;
  /** Replays the inverse of the most recent `done` entry; restores its
   *  carried `selBefore`. No-op when `done` is empty. */
  undo(): void;
  /** Reapplies the most recent `undone` entry; restores its carried
   *  `selAfter`. No-op when `undone` is empty. */
  redo(): void;
  /** NFR-EDITOR-003: forces a coalescing boundary so the next `apply`
   *  starts a new `done` entry even if it arrives inside the coalescing
   *  window. */
  closeGroup(): void;
  /** NFR-EDITOR-003: applies an op without pushing a `done` entry
   *  (programmatic / remote edits opt out of history). */
  applyExcluded(op: EditorOp): void;
}

export interface EditorHistory {
  readonly memory: Memory<EditorHistoryState>;
  /** Reads `memory.get().done.length > 0` at call time -- a computed
   *  accessor, NEVER a sibling field written via `memory.set`/`patch`. */
  readonly canUndo: boolean;
  readonly canRedo: boolean;
  readonly controls: EditorHistoryControls;
}

// ---------------------------------------------------------------------------
// Small local helpers -- generic InlineContent[]/selection arithmetic, not a
// reimplementation of block-operations' structural logic or
// inline-formatter's mark vocabulary. Structural/mark edits themselves are
// always delegated to applyOp; these only read positions/lengths to decide
// what op to synthesize (replace-selection) or where the caret lands after
// an already-applied op.
// ---------------------------------------------------------------------------

function normalizeContent(content: string | InlineContent[] | undefined): InlineContent[] {
  if (content === undefined) return [];
  if (typeof content === "string") return content.length === 0 ? [] : [{ text: content }];
  return content;
}

function contentLength(content: InlineContent[]): number {
  return content.reduce((sum, run) => sum + run.text.length, 0);
}

/** Slice an InlineContent[] run array to the character range [start, end),
 *  preserving marks/href per run -- the same per-run overlap that two
 *  successive splits (block-operations' splitInlineContent, composed by
 *  ops/text.ts's removeText) would produce, so a removeText op built from
 *  this slice matches removeText's own exact-equality check against the
 *  block's actual content. */
function sliceContent(
  content: string | InlineContent[] | undefined,
  start: number,
  end: number,
): InlineContent[] {
  const runs = normalizeContent(content);
  const result: InlineContent[] = [];
  let pos = 0;
  for (const run of runs) {
    const runStart = pos;
    const runEnd = pos + run.text.length;
    pos = runEnd;
    const overlapStart = Math.max(start, runStart);
    const overlapEnd = Math.min(end, runEnd);
    if (overlapStart >= overlapEnd) continue;
    const piece: InlineContent = {
      text: run.text.slice(overlapStart - runStart, overlapEnd - runStart),
    };
    if (run.marks && run.marks.length > 0) piece.marks = [...run.marks];
    if (run.href !== undefined) piece.href = run.href;
    result.push(piece);
  }
  return result;
}

function collapsedAt(blockId: string, offset: number): EditorSelection {
  const pos: EditorPosition = { blockId, offset };
  return { anchor: pos, focus: pos };
}

function isCollapsed(sel: EditorSelection): boolean {
  return sel.anchor.blockId === sel.focus.blockId && sel.anchor.offset === sel.focus.offset;
}

function orderedRange(sel: EditorSelection): [number, number] {
  const a = sel.anchor.offset;
  const b = sel.focus.offset;
  return a <= b ? [a, b] : [b, a];
}

/** Derive the caret/selection after one already-applied op. Text ops move
 *  the caret to the edit boundary; format ops keep the same range (marks
 *  restyle text without moving it); structural ops that create or reunite
 *  blocks land the caret at the resulting boundary, read off the op's own
 *  pre-assigned id (split) or its OpResult.inverse (merge/delete, which
 *  carry the boundary/anchor position the reverse op would target -- the
 *  exact position their forward application produced). Ops that do not
 *  move the caret (convert) or have no single natural landing spot in this
 *  issue's scope (bare insert) keep the selection as it was. */
function selectionAfterOp(
  op: EditorOp,
  result: OpResult,
  selBefore: EditorSelection,
): EditorSelection {
  switch (op.kind) {
    case "insertText":
      return collapsedAt(op.blockId, op.offset + contentLength(op.text));
    case "removeText":
      return collapsedAt(op.blockId, op.offset);
    case "applyMark":
    case "removeMark":
      return {
        anchor: { blockId: op.blockId, offset: op.start },
        focus: { blockId: op.blockId, offset: op.end },
      };
    case "split":
      return collapsedAt(op.newBlockId, 0);
    case "mergePrev":
    case "mergeNext": {
      const boundary = result.inverse[0];
      if (boundary?.kind === "removeText") return collapsedAt(boundary.blockId, boundary.offset);
      return selBefore;
    }
    case "delete": {
      const reinstate = result.inverse[0];
      if (reinstate?.kind === "insert") return collapsedAt(reinstate.atBlockId, reinstate.atOffset);
      return selBefore;
    }
    case "convert":
      return selBefore;
    case "insert": {
      const last = op.blocks[op.blocks.length - 1];
      if (last) return collapsedAt(last.id, contentLength(normalizeContent(last.content)));
      return selBefore;
    }
    default: {
      const exhaustive: never = op;
      throw new Error(`selectionAfterOp: unknown op kind "${(exhaustive as EditorOp).kind}"`);
    }
  }
}

/** Whether `next` should coalesce into the same HistoryEntry as `prev`
 *  (NFR-EDITOR-003) -- only consecutive text ops on the same block at
 *  adjacent offsets (a run of typed or backspaced characters). Structural
 *  and format ops always start a new entry. */
function isCoalescible(prev: EditorOp, next: EditorOp): boolean {
  if (prev.kind === "insertText" && next.kind === "insertText") {
    return prev.blockId === next.blockId && prev.offset + contentLength(prev.text) === next.offset;
  }
  if (prev.kind === "removeText" && next.kind === "removeText") {
    const nextLen = contentLength(next.text);
    return (
      prev.blockId === next.blockId &&
      // backspace run (deleting leftward): each removal's end meets the previous one's start.
      (next.offset + nextLen === prev.offset ||
        // forward-delete run (Delete key): offset stays put as content shifts left under it.
        next.offset === prev.offset)
    );
  }
  return false;
}

/** Callers seed doc/sel; done/undone always start empty -- a caller cannot
 *  construct a history with a pre-populated op-log. */
export function createEditorHistory(
  initial?: Pick<EditorHistoryState, "doc" | "sel">,
  config?: EditorHistoryConfig,
): EditorHistory {
  const cap = config?.cap ?? 100;
  const coalesceWindowMs = config?.coalesceWindowMs ?? 500;

  const seedDoc = initial?.doc ?? [];
  const seedSel = initial?.sel ?? collapsedAt("", 0);

  const memory = createMemory<EditorHistoryState>(() => ({
    doc: seedDoc,
    sel: seedSel,
    done: [],
    undone: [],
  }));

  // Coalescing bookkeeping (NFR-EDITOR-003). Ephemeral control-plane state,
  // not part of the document/history -- it never lives in the cell.
  let lastCommitAt = 0;
  let forceNewGroup = true;

  function commitEntry(opsToApply: readonly EditorOp[]): void {
    const state = memory.get();
    const selBefore = state.sel;

    let blocks = state.doc;
    const perOpInverses: EditorOp[][] = [];
    let lastResult: OpResult | undefined;
    for (const op of opsToApply) {
      const result = applyOp(blocks, op);
      blocks = result.blocks;
      perOpInverses.push(result.inverse);
      lastResult = result;
    }
    const primaryOp = opsToApply[opsToApply.length - 1] as EditorOp;
    const selAfter = lastResult ? selectionAfterOp(primaryOp, lastResult, selBefore) : selBefore;

    // Flatten: for each op in reverse order, its own inverse sequence
    // (already in the order applyOp expects it applied).
    const inverses: EditorOp[] = [];
    for (let i = perOpInverses.length - 1; i >= 0; i--) {
      inverses.push(...(perOpInverses[i] as EditorOp[]));
    }

    const now = Date.now();
    const prevEntry = state.done[state.done.length - 1];
    const canCoalesce =
      !forceNewGroup &&
      opsToApply.length === 1 &&
      prevEntry !== undefined &&
      now - lastCommitAt <= coalesceWindowMs &&
      isCoalescible(prevEntry.ops[prevEntry.ops.length - 1] as EditorOp, primaryOp);

    let done: HistoryEntry[];
    if (canCoalesce && prevEntry) {
      const merged: HistoryEntry = {
        ops: [...prevEntry.ops, ...opsToApply],
        inverses: [...inverses, ...prevEntry.inverses],
        selBefore: prevEntry.selBefore,
        selAfter,
      };
      done = [...state.done.slice(0, -1), merged];
    } else {
      const entry: HistoryEntry = { ops: [...opsToApply], inverses, selBefore, selAfter };
      done = [...state.done, entry];
      if (done.length > cap) done = done.slice(done.length - cap);
    }

    lastCommitAt = now;
    forceNewGroup = false;

    memory.set({ doc: blocks, sel: selAfter, done, undone: [] });
  }

  const controls: EditorHistoryControls = {
    apply(op: EditorOp): void {
      const state = memory.get();
      const sel = state.sel;
      const opsToApply: EditorOp[] = [];

      if (
        op.kind === "insertText" &&
        !isCollapsed(sel) &&
        sel.anchor.blockId === sel.focus.blockId &&
        sel.anchor.blockId === op.blockId
      ) {
        const [start, end] = orderedRange(sel);
        const block = state.doc.find((b) => b.id === op.blockId);
        const removed = block ? sliceContent(block.content, start, end) : [];
        opsToApply.push({ kind: "removeText", blockId: op.blockId, offset: start, text: removed });
      }
      opsToApply.push(op);

      commitEntry(opsToApply);
    },

    undo(): void {
      const state = memory.get();
      const entry = state.done[state.done.length - 1];
      if (!entry) return; // no-op, per canDispatch-style gate-before-apply discipline
      const doc = applyOpSequence(state.doc, [...entry.inverses]);
      const done = state.done.slice(0, -1);
      const undone = [...state.undone, entry];
      forceNewGroup = true;
      memory.set({ doc, sel: entry.selBefore, done, undone });
    },

    redo(): void {
      const state = memory.get();
      const entry = state.undone[state.undone.length - 1];
      if (!entry) return; // no-op, per canDispatch-style gate-before-apply discipline
      const doc = applyOpSequence(state.doc, [...entry.ops]);
      const undone = state.undone.slice(0, -1);
      const done = [...state.done, entry];
      forceNewGroup = true;
      memory.set({ doc, sel: entry.selAfter, done, undone });
    },

    closeGroup(): void {
      forceNewGroup = true;
    },

    applyExcluded(op: EditorOp): void {
      const state = memory.get();
      const result = applyOp(state.doc, op);
      const selAfter = selectionAfterOp(op, result, state.sel);
      // A programmatic/remote edit is never part of the same undo step as
      // the surrounding user edits -- without this, a keystroke right
      // after an excluded edit could coalesce into a `done` entry whose
      // position-based inverses were computed against a doc state the
      // excluded edit has since changed underneath it.
      forceNewGroup = true;
      memory.set({ doc: result.blocks, sel: selAfter, done: state.done, undone: state.undone });
    },
  };

  return {
    memory,
    get canUndo(): boolean {
      return memory.get().done.length > 0;
    },
    get canRedo(): boolean {
      return memory.get().undone.length > 0;
    },
    controls,
  };
}
