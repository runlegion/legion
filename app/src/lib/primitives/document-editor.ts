/**
 * Document editor composition primitive - unified contentEditable surface
 *
 * Orchestrates leaf primitives into a document editing experience:
 * - input-events: content change detection with IME support
 * - clipboard: paste/copy/cut with format detection
 * - keyboard-handler: block type shortcuts (Cmd+Alt+1, etc.)
 * - cursor-tracker: cursor position relative to blocks
 * - block-operations: split/merge/delete/convert (pure functions)
 * - inline-formatter: bold/italic/code/etc
 * - editor-history (FR-EDITOR-002): undo/redo, op-based
 * - serializer-html/text: paste deserialization, copy serialization
 *
 * The React component layer is a thin wrapper: state + render. This primitive
 * owns all event handling and block mutation logic.
 *
 * @registry-name document-editor
 * @registry-version 0.1.0
 * @registry-status published
 * @registry-path primitives/document-editor.ts
 * @registry-type registry:primitive
 *
 * @dependencies nanostores
 * @internal-dependencies primitives/input-events.ts, primitives/clipboard.ts,
 *   primitives/keyboard-handler.ts, primitives/cursor-tracker.ts,
 *   primitives/block-operations.ts, components/editor/editor-history.ts,
 *   components/editor/ops, primitives/serializer-html.ts, primitives/serializer-text.ts
 */
import {
  blockContentToText,
  deleteBlock,
  mergeWithNext,
  mergeWithPrevious,
} from "@/lib/primitives/block-operations";
import { createClipboard } from "@/lib/primitives/clipboard";
import {
  findBlockElement,
  getCursorPosition,
  isCursorAtBlockEnd,
  isSelectionCollapsed,
  setCursorAtBlockEnd,
  setCursorAtBlockStart,
  setCursorInBlock,
} from "@/lib/primitives/cursor-tracker";
import { createEditorHistory } from "@/components/ui/components/editor/editor-history";
import { normalizeRuns } from "@/components/ui/components/editor/ops/content";
import type { EditorOp } from "@/components/ui/components/editor/ops";
import { createInputHandler } from "@/lib/primitives/input-events";
import { createKeyboardHandler } from "@/lib/primitives/keyboard-handler";
import { createMemory } from "@/lib/primitives/memory";
import { htmlSerializer } from "@/lib/primitives/serializer-html";
import { textSerializer } from "@/lib/primitives/serializer-text";
import type { BaseBlock, CleanupFunction } from "@/lib/primitives/types";

// =============================================================================
// Types
// =============================================================================

export interface DocumentEditorState {
  blocks: BaseBlock[];
  canUndo: boolean;
  canRedo: boolean;
}

export interface DocumentEditorOptions {
  /** Container element that becomes contentEditable */
  container: HTMLElement;
  /** Initial blocks */
  initialBlocks: BaseBlock[];
  /** Called when blocks change */
  onBlocksChange?: (blocks: BaseBlock[]) => void;
}

export interface DocumentEditorControls {
  /** Reactive state atom */
  $state: {
    get(): DocumentEditorState;
    subscribe(cb: (v: DocumentEditorState) => void): () => void;
  };
  /** Replace blocks (e.g., from external load/import) */
  setBlocks: (blocks: BaseBlock[]) => void;
  /** Add blocks at a position */
  addBlocks: (blocks: BaseBlock[], index?: number) => void;
  /** Undo */
  undo: () => void;
  /** Redo */
  redo: () => void;
  /** Focus a block at an offset */
  focusBlock: (blockId: string, offset?: number) => void;
  /** Destroy all listeners */
  destroy: CleanupFunction;
}

// =============================================================================
// Markdown shortcuts (detected on space after prefix)
// =============================================================================

interface MarkdownShortcut {
  pattern: RegExp;
  type: string;
  meta?: Record<string, unknown>;
}

const MARKDOWN_SHORTCUTS: MarkdownShortcut[] = [
  { pattern: /^####$/, type: "heading", meta: { level: 4 } },
  { pattern: /^###$/, type: "heading", meta: { level: 3 } },
  { pattern: /^##$/, type: "heading", meta: { level: 2 } },
  { pattern: /^#$/, type: "heading", meta: { level: 1 } },
  { pattern: /^>$/, type: "quote" },
  { pattern: /^[-*]$/, type: "list-item", meta: { listType: "unordered" } },
  { pattern: /^\d+\.$/, type: "list-item", meta: { listType: "ordered" } },
  { pattern: /^```$/, type: "code" },
  { pattern: /^---$/, type: "divider" },
];

// =============================================================================
// Implementation
// =============================================================================

export function createDocumentEditor(options: DocumentEditorOptions): DocumentEditorControls {
  const { container, initialBlocks, onBlocksChange } = options;
  const cleanups: CleanupFunction[] = [];

  // -- Shared state --
  // FR-EDITOR-002's op-based history (RULING-EDITOR-HISTORY): the OLD
  // primitives/history.ts (snapshot push/pop) is gone from this file's
  // dependency path. `history.memory`'s own `doc` field is this editor's
  // canonical block array; `state` below mirrors it into the
  // DocumentEditorState shape this primitive's public API already commits to
  // (blocks/canUndo/canRedo), via `syncFromHistory`.
  const firstId = initialBlocks[0]?.id ?? "";
  const initialPos = { blockId: firstId, offset: 0 };
  const history = createEditorHistory(
    { doc: initialBlocks, sel: { anchor: initialPos, focus: initialPos } },
    { cap: 100 },
  );

  const state = createMemory<DocumentEditorState>({
    blocks: initialBlocks,
    canUndo: false,
    canRedo: false,
  });

  function syncFromHistory(): void {
    const next = history.memory.get().doc;
    state.set({
      blocks: next,
      canUndo: history.canUndo,
      canRedo: history.canRedo,
    });
    onBlocksChange?.(next);
  }

  /** Apply one EditorOp through FR-EDITOR-002's history (one `done` entry),
   *  WITHOUT publishing -- the caller decides when to call `syncFromHistory`.
   *  `applyOp` (FR-EDITOR-003) throws on an unresolvable op -- a stale
   *  blockId or an out-of-bounds offset, which can happen when the DOM has
   *  drifted from the model between reconciles. Mirrors editor.behavior.ts's
   *  `applyOps` discipline: skip, leave the cell at its last valid state, let
   *  the next reconcile re-sync from the DOM. Typing must never throw.
   *  Returns whether the op actually applied. */
  function commitOpSilent(op: EditorOp): boolean {
    try {
      history.controls.apply(op);
      return true;
    } catch {
      return false;
    }
  }

  /** Apply one EditorOp and publish immediately -- the single-op call sites
   *  below (Enter, Backspace, Delete, block-type shortcuts, paste) each
   *  represent one user action, one `done` entry, one `onBlocksChange`. */
  function commitOp(op: EditorOp): void {
    if (commitOpSilent(op)) syncFromHistory();
  }

  /** Diff `nextBlocks` (freshly read off the DOM by `reconcileDOM`) against
   *  the history's current `doc` and commit the difference as EditorOps: a
   *  `delete` per removed block, a whole-block `removeText` + `insertText`
   *  pair per content-changed block. `reconcileDOM` only ever drops or
   *  edits blocks already in the model (never introduces a new id -- see its
   *  own comment), so no `insert` case is needed here.
   *
   *  This is a coarser undo grain than a real keystroke-level diff (two
   *  `done` entries per changed block instead of one), because
   *  FR-EDITOR-003's vocabulary has no "replace this block's content"
   *  primitive -- but every commit still lands on FR-EDITOR-002's op-log, so
   *  typing driven through this DOM-first reconciliation path stays
   *  undoable, which a raw `memory.set` bypass would not preserve.
   *
   *  Every op in one reconcile is applied via `commitOpSilent` (no publish),
   *  then `syncFromHistory` runs ONCE at the end, iff anything actually
   *  applied. Publishing after each op would expose the cell's INTERMEDIATE
   *  state to `onBlocksChange` -- a changed block sits with empty content
   *  between its `removeText` and `insertText` -- which a persisting
   *  consumer (`onBlocksChange={(blocks) => save(blocks)}`, per
   *  `old/ui/editor.tsx`'s own doc comment) would write on every keystroke.
   *  The original snapshot-push `updateBlocks` fired the callback exactly
   *  once per reconcile; this preserves that. */
  function commitReconciled(nextBlocks: BaseBlock[]): void {
    const prevBlocks = history.memory.get().doc;
    const nextIds = new Set(nextBlocks.map((b) => b.id));
    let changed = false;

    for (const prev of prevBlocks) {
      if (!nextIds.has(prev.id)) {
        if (commitOpSilent({ kind: "delete", blockId: prev.id })) changed = true;
      }
    }

    const prevById = new Map(prevBlocks.map((b) => [b.id, b]));
    for (const next of nextBlocks) {
      const prev = prevById.get(next.id);
      if (!prev) continue;
      if (blockContentToText(prev.content) === blockContentToText(next.content)) continue;

      const removed = normalizeRuns(prev.content);
      if (removed.length > 0) {
        if (commitOpSilent({ kind: "removeText", blockId: next.id, offset: 0, text: removed })) {
          changed = true;
        }
      }
      const inserted = normalizeRuns(next.content);
      if (inserted.length > 0) {
        if (commitOpSilent({ kind: "insertText", blockId: next.id, offset: 0, text: inserted })) {
          changed = true;
        }
      }
    }

    if (changed) syncFromHistory();
  }

  // -- Make container contentEditable --
  container.setAttribute("contenteditable", "true");
  container.setAttribute("spellcheck", "true");
  container.setAttribute("role", "textbox");
  container.setAttribute("aria-multiline", "true");
  container.setAttribute("aria-label", "Document editor");
  cleanups.push(() => {
    container.removeAttribute("contenteditable");
    container.removeAttribute("spellcheck");
    container.removeAttribute("role");
    container.removeAttribute("aria-multiline");
    container.removeAttribute("aria-label");
  });

  // -- DOM reconciliation: read block elements and sync to model --
  function reconcileDOM(): void {
    const blockEls = container.querySelectorAll("[data-block-id]");
    const blocks = state.get().blocks;
    const blockMap = new Map(blocks.map((b) => [b.id, b]));

    // Build new block list from DOM order
    const reconciled: BaseBlock[] = [];
    const seen = new Set<string>();

    for (const el of blockEls) {
      const id = el.getAttribute("data-block-id");
      if (!id || seen.has(id)) continue;
      seen.add(id);

      const existing = blockMap.get(id);
      if (existing) {
        // Update content from DOM
        const text = el.textContent ?? "";
        if (blockContentToText(existing.content) !== text) {
          reconciled.push({ ...existing, content: text });
        } else {
          reconciled.push(existing);
        }
      }
    }

    // Only update if something changed
    if (
      reconciled.length !== blocks.length ||
      reconciled.some((b, i) => {
        const orig = blocks[i];
        return (
          !orig ||
          b.id !== orig.id ||
          blockContentToText(b.content) !== blockContentToText(orig.content)
        );
      })
    ) {
      commitReconciled(reconciled);
    }
  }

  // -- Input events: sync DOM text changes back to block model --
  const inputHandler = createInputHandler({
    element: container,
    onInput: (data) => {
      if (data.isComposing) return;

      const pos = getCursorPosition();
      if (!pos) return;

      // insertParagraph = Enter key (browser already split the DOM)
      if (data.inputType === "insertParagraph") {
        const newBlockId = crypto.randomUUID();
        commitOp({ kind: "split", blockId: pos.blockId, offset: pos.offset, newBlockId });
        // Focus the new block after React re-renders
        requestAnimationFrame(() => {
          setCursorAtBlockStart(container, newBlockId);
        });
        return;
      }

      // Reconcile DOM with block model after any mutation.
      // The browser may have deleted blocks (cross-block selection delete),
      // merged text across blocks, or modified content.
      reconcileDOM();
    },
    onBeforeInput: (data) => {
      // Detect markdown shortcuts: check if text before cursor + typed char matches
      if (data.inputType === "insertText" && data.data === " ") {
        const pos = getCursorPosition();
        if (!pos) return;

        const blocks = state.get().blocks;
        const block = blocks.find((b) => b.id === pos.blockId);
        if (!block || block.type !== "text") return;

        const textBeforeCursor = blockContentToText(block.content).slice(0, pos.offset);
        for (const shortcut of MARKDOWN_SHORTCUTS) {
          if (shortcut.pattern.test(textBeforeCursor)) {
            // Prevent the space from being inserted
            // Note: we return here, the beforeinput handler in input-events
            // will check the preventDefault array
            commitOp({
              kind: "convert",
              blockId: pos.blockId,
              newType: shortcut.type,
              ...(shortcut.meta ? { meta: shortcut.meta } : {}),
            });
            // Clear the prefix content -- read the CONVERTED block back (not
            // the pre-convert one) since convertBlockType may itself have
            // transformed content (e.g. code blocks flatten marks); this must
            // match what's actually in the cell or removeText throws.
            const converted = state.get().blocks.find((b) => b.id === pos.blockId);
            const prefixRuns = normalizeRuns(converted?.content);
            if (prefixRuns.length > 0) {
              commitOp({
                kind: "removeText",
                blockId: pos.blockId,
                offset: 0,
                text: prefixRuns,
              });
            }
            requestAnimationFrame(() => {
              setCursorAtBlockStart(container, pos.blockId);
            });
            return;
          }
        }
      }
    },
    preventDefault: ["insertParagraph"],
  });
  cleanups.push(inputHandler.cleanup);

  // -- Keyboard: Backspace at start, Delete at end --
  const backspaceCleanup = createKeyboardHandler(container, {
    key: "Backspace",
    handler: () => {
      if (!isSelectionCollapsed()) return; // Let browser handle selection delete

      const pos = getCursorPosition();
      if (!pos) return;

      const blocks = state.get().blocks;
      const index = blocks.findIndex((b) => b.id === pos.blockId);
      const block = blocks[index];
      if (!block) return;

      // Only intercept at position 0
      if (pos.offset !== 0) return;

      // Empty block: delete it
      if (pos.blockLength === 0 && index > 0) {
        const result = deleteBlock(blocks, pos.blockId);
        commitOp({ kind: "delete", blockId: pos.blockId });
        if (result.focusBlockId) {
          requestAnimationFrame(() => {
            if (result.focusAtEnd) {
              setCursorAtBlockEnd(container, result.focusBlockId as string);
            } else {
              setCursorAtBlockStart(container, result.focusBlockId as string);
            }
          });
        }
        return;
      }

      // Heading/quote at start: convert to text
      if (block.type === "heading" || block.type === "quote") {
        commitOp({ kind: "convert", blockId: pos.blockId, newType: "text" });
        requestAnimationFrame(() => {
          setCursorAtBlockStart(container, pos.blockId);
        });
        return;
      }

      // Merge with previous
      if (index > 0) {
        const result = mergeWithPrevious(blocks, pos.blockId);
        commitOp({ kind: "mergePrev", blockId: pos.blockId });
        requestAnimationFrame(() => {
          setCursorInBlock(container, result.survivorId, result.cursorOffset);
        });
      }
    },
    preventDefault: false, // We conditionally prevent in the handler
  });
  cleanups.push(backspaceCleanup);

  const deleteCleanup = createKeyboardHandler(container, {
    key: "Delete",
    handler: () => {
      if (!isSelectionCollapsed()) return;
      if (!isCursorAtBlockEnd()) return;

      const pos = getCursorPosition();
      if (!pos) return;

      const blocks = state.get().blocks;
      const result = mergeWithNext(blocks, pos.blockId);
      commitOp({ kind: "mergeNext", blockId: pos.blockId });
      requestAnimationFrame(() => {
        setCursorInBlock(container, result.survivorId, result.cursorOffset);
      });
    },
    preventDefault: false,
  });
  cleanups.push(deleteCleanup);

  // -- Keyboard: block type shortcuts (Cmd+Alt+0 through Cmd+Alt+4, etc.) --
  // These use the keyboard-handler primitive for clean binding
  // Cmd+Alt+N shortcuts for block type switching
  // Uses raw keydown because keyboard-handler only supports KeyboardKey types (not digits)
  function handleTypeShortcut(event: KeyboardEvent): void {
    if (!event.altKey || !(event.metaKey || event.ctrlKey)) return;

    const pos = getCursorPosition();
    if (!pos) return;

    let op: EditorOp | null = null;

    switch (event.key) {
      case "0":
        op = { kind: "convert", blockId: pos.blockId, newType: "text" };
        break;
      case "1":
        op = { kind: "convert", blockId: pos.blockId, newType: "heading", meta: { level: 1 } };
        break;
      case "2":
        op = { kind: "convert", blockId: pos.blockId, newType: "heading", meta: { level: 2 } };
        break;
      case "3":
        op = { kind: "convert", blockId: pos.blockId, newType: "heading", meta: { level: 3 } };
        break;
      case "4":
        op = { kind: "convert", blockId: pos.blockId, newType: "heading", meta: { level: 4 } };
        break;
    }

    if (op) {
      event.preventDefault();
      commitOp(op);
    }
  }

  container.addEventListener("keydown", handleTypeShortcut);
  cleanups.push(() => container.removeEventListener("keydown", handleTypeShortcut));

  // -- Clipboard: paste with format detection, copy with serialization --
  const clipboard = createClipboard({
    container,
    customMimeType: "application/x-rafters-blocks",
    onPaste: (data) => {
      const pos = getCursorPosition();
      if (!pos) return;

      const blocks = state.get().blocks;
      const block = blocks.find((b) => b.id === pos.blockId);

      // In code blocks, paste as plain text (let browser handle it)
      if (block?.type === "code") return;

      // Deserialize: try HTML first, fall back to plain text
      let pastedBlocks: BaseBlock[];
      if (data.html?.trim()) {
        pastedBlocks = htmlSerializer.deserialize(data.html).blocks;
      } else if (data.text) {
        pastedBlocks = textSerializer.deserialize(data.text).blocks;
      } else {
        return;
      }

      if (pastedBlocks.length === 0) return;

      // Single text block: insert text inline (don't create new block)
      if (pastedBlocks.length === 1) {
        const pasted = pastedBlocks[0];
        if (pasted && pasted.type === "text") {
          const pastedText = blockContentToText(pasted.content);
          document.execCommand("insertText", false, pastedText);
          return;
        }
      }

      // Multiple blocks: insert at cursor position. `lastInsertedId` is
      // always the last element of `pastedBlocks` (insertBlocksAt's own
      // convention -- see block-operations.ts) regardless of whether a
      // mid-block split occurs, so no extra call is needed to learn it.
      const lastInsertedId = pastedBlocks[pastedBlocks.length - 1]?.id ?? pos.blockId;
      commitOp({
        kind: "insert",
        blocks: pastedBlocks,
        atBlockId: pos.blockId,
        atOffset: pos.offset,
        splitBlockId: crypto.randomUUID(),
      });
      requestAnimationFrame(() => {
        setCursorAtBlockEnd(container, lastInsertedId);
      });
    },
    onCopy: () => {
      // Multi-block copy is handled by the copy event listener below
    },
  });
  cleanups.push(clipboard.cleanup);

  // -- Copy/Cut: serialize selected blocks --
  function handleCopy(event: ClipboardEvent): void {
    const sel = document.getSelection();
    if (!sel || sel.isCollapsed) return;

    const anchorBlock = findBlockElement(sel.anchorNode);
    const focusBlock = findBlockElement(sel.focusNode);
    if (!anchorBlock || !focusBlock) return;

    // Within a single block: let browser handle it
    if (anchorBlock === focusBlock) return;

    event.preventDefault();

    const blocks = state.get().blocks;
    const anchorId = anchorBlock.getAttribute("data-block-id") ?? "";
    const focusId = focusBlock.getAttribute("data-block-id") ?? "";
    const anchorIdx = blocks.findIndex((b) => b.id === anchorId);
    const focusIdx = blocks.findIndex((b) => b.id === focusId);
    const startIdx = Math.min(anchorIdx, focusIdx);
    const endIdx = Math.max(anchorIdx, focusIdx);

    if (startIdx === -1 || endIdx === -1) return;

    const selected = blocks.slice(startIdx, endIdx + 1);
    event.clipboardData?.setData("text/html", htmlSerializer.serialize(selected));
    event.clipboardData?.setData("text/plain", textSerializer.serialize(selected));
  }

  container.addEventListener("copy", handleCopy);
  cleanups.push(() => container.removeEventListener("copy", handleCopy));

  container.addEventListener("cut", (event: ClipboardEvent) => {
    handleCopy(event);
    // Let browser delete the selected content, input handler will sync
  });

  // -- Public API --
  function setBlocks(blocks: BaseBlock[]): void {
    // A wholesale replace (external load/import) has no natural EditorOp:
    // every op in FR-EDITOR-003's vocabulary anchors on an EXISTING block,
    // which an arbitrary externally-supplied document may not share with the
    // current one at all. Reset the cell directly through its public
    // `memory` -- editor-history.ts exposes a Memory<EditorHistoryState> for
    // exactly this kind of non-incremental write -- and clear the op-log:
    // there is no derived op sequence back to the replaced document, so it
    // is not meaningfully "undoable" through this history.
    const firstBlockId = blocks[0]?.id ?? "";
    const pos = { blockId: firstBlockId, offset: 0 };
    history.memory.set({ doc: blocks, sel: { anchor: pos, focus: pos }, done: [], undone: [] });
    syncFromHistory();
  }

  function addBlocks(newBlocks: BaseBlock[], index?: number): void {
    // Same rationale as setBlocks: the target document may currently be
    // empty (no anchor block for FR-EDITOR-003's `insert` op) or the insert
    // may span a position `insert` doesn't address (append past the end);
    // expressed directly on the memory cell rather than fabricated as ops.
    const current = history.memory.get();
    const next = [...current.doc];
    if (index !== undefined && index >= 0 && index <= next.length) {
      next.splice(index, 0, ...newBlocks);
    } else {
      next.push(...newBlocks);
    }
    history.memory.set({ ...current, doc: next });
    syncFromHistory();
  }

  function undo(): void {
    if (!history.canUndo) return;
    history.controls.undo();
    syncFromHistory();
  }

  function redo(): void {
    if (!history.canRedo) return;
    history.controls.redo();
    syncFromHistory();
  }

  function focusBlock(blockId: string, offset?: number): void {
    if (offset !== undefined) {
      setCursorInBlock(container, blockId, offset);
    } else {
      setCursorAtBlockStart(container, blockId);
    }
  }

  return {
    $state: state.atom,
    setBlocks,
    addBlocks,
    undo,
    redo,
    focusBlock,
    destroy() {
      for (const cleanup of cleanups) cleanup();
    },
  };
}
