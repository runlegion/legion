/**
 * editor.behavior.ts -- controlled contenteditable binding (FR-EDITOR-004).
 *
 * `bindEditor` intercepts `beforeinput`, translates each input into an
 * `EditorOp` applied through the editor history's `controls`, `preventDefault`s
 * so the MODEL owns the edit (the browser never mutates the contenteditable),
 * projects model state -> DOM (`projectDocument`), and restores the DOM
 * selection from `state.sel` after every render.
 *
 * Per RULING-EDITOR-HISTORY's pinned "Editor interface contract" this file
 * composes `createEditorHistory` (FR-EDITOR-002) and `applyOp`'s op vocabulary
 * (FR-EDITOR-003) DIRECTLY -- there is NO import of `compose`, `createBehavior`,
 * or `BehaviorSpec`; the editor is out of the behavior-layer composer (frozen
 * Spec 00). It borrows only the Spec 05 `bindX` LIFECYCLE (read config off
 * `root`'s `data-*`, subscribe to the memory cell, render on change, restore
 * selection, return a teardown).
 *
 * FR-EDITOR-005 adds the editor SCORE below (`parts`, `editorAria`,
 * `editorKeymap`) -- hand-written, not produced by `compose()` -- and extends
 * `bindEditor` with the aria projection and the undo/redo keymap wiring.
 */
import { z } from "zod";
import type { AriaAttrs, KeyInput, PartDecl, PartIds } from "@/lib/contract";
import { updateAriaAttribute } from "@/lib/primitives/aria-manager";
import { createClipboard } from "@/lib/primitives/editor/clipboard";
import { createInputHandler } from "@/lib/primitives/editor/input-events";
import type { BaseBlock, InlineContent } from "@/lib/primitives/types";
import type { EditorHistory, EditorHistoryState } from "@/components/ui/editor-history";
import { createEditorHistory } from "@/components/ui/editor-history";
import { normalizeRuns, splitRuns, totalTextLength } from "@/components/ui/ops/content";
import type { EditorOp } from "@/components/ui/ops";

// ---------------------------------------------------------------------------
// The editor score (FR-EDITOR-005): parts, editorAria, editorKeymap. Root-only
// (settled) -- the contenteditable owns block DOM internally, no per-block
// parts. Composed directly by `bindEditor` below; this is NOT a
// compose()/BehaviorSpec (frozen Spec 00 line 132) -- this file imports
// neither `compose` nor `createBehavior` and declares no `Slice`.
// ---------------------------------------------------------------------------

export type EditorPart = "root";

/** The one memory cell's shape (RULING-EDITOR-HISTORY interface contract,
 *  point 2): `doc: BaseBlock[]`, plus FR-EDITOR-002's `sel`/`done`/`undone` --
 *  re-exported from `editor-history.ts`'s own type rather than retyped. */
export type EditorState = EditorHistoryState;

/** Accessible-name is REQUIRED, not optional: axe fails a role=textbox with
 *  no name, so the type forces one of the two rather than leaving it to
 *  review. */
export type EditorLabelConfig =
  | { label: string; labelledBy?: undefined }
  | { label?: undefined; labelledBy: string };

export type EditorConfig = EditorLabelConfig & {
  disabled?: boolean | undefined;
  readonly?: boolean | undefined;
};

/** Root-only (settled): the contenteditable owns block DOM internally -- no
 *  per-block parts. */
export const parts: Record<EditorPart, PartDecl> = {
  root: { role: "textbox" },
};

/** Pure ARIA projection, same signature shape as every other score's `aria`
 *  field but hand-written -- not produced by compose(). `ids` completes the
 *  shared signature; unused here because the editor's root never
 *  cross-references another part's id. */
export function editorAria(
  _state: EditorState,
  config: EditorConfig,
  _ids: PartIds<EditorPart>,
): Partial<Record<EditorPart, AriaAttrs>> {
  return {
    root: {
      role: "textbox",
      "aria-multiline": "true",
      "aria-label": config.label,
      "aria-labelledby": config.labelledBy,
    },
  };
}

/**
 * Pure keymap projection. Claims undo/redo on BOTH the Mac chord and the
 * Windows/Linux chord. `KeyboardEvent.key` reports the shifted character
 * (`'Z'`, not `'z'`) when Shift is held, so the comparison lowercases first --
 * otherwise the redo chord silently never matches in a real browser. Every
 * other key -- including plain character input -- returns null: content
 * edits are NOT routed through keymap, they arrive via beforeinput
 * (FR-EDITOR-004), composed by bindEditor.
 */
export function editorKeymap(
  event: KeyInput,
  _state: EditorState,
  _part: EditorPart,
  _config: EditorConfig,
): "undo" | "redo" | null {
  if (event.key.toLowerCase() !== "z") return null;
  if (!(event.metaKey || event.ctrlKey)) return null;
  return event.shiftKey ? "redo" : "undo";
}

// ---------------------------------------------------------------------------
// Config parsing (external data off `root`'s data-* attributes -> Zod).
// ---------------------------------------------------------------------------

const inlineContentSchema = z.object({
  text: z.string(),
  marks: z.array(z.enum(["bold", "italic", "code", "strikethrough", "link"])).optional(),
  href: z.string().optional(),
});

const baseBlockSchema = z.object({
  id: z.string(),
  type: z.string(),
  content: z.union([z.string(), z.array(inlineContentSchema)]).optional(),
  children: z.array(z.string()).optional(),
  parentId: z.string().optional(),
  meta: z.record(z.string(), z.unknown()).optional(),
});

const docSchema = z.array(baseBlockSchema);
const caretSchema = z.object({ blockId: z.string(), offset: z.number() });

function parseSeed(root: HTMLElement): Pick<EditorHistoryState, "doc" | "sel"> {
  const rawDoc = root.dataset.initialDoc;
  // Zod validates the external shape; the inferred `optional` fields widen to
  // `... | undefined`, so narrow back to BaseBlock[] for the history seed.
  const doc = (rawDoc ? docSchema.parse(JSON.parse(rawDoc)) : []) as BaseBlock[];

  const rawCaret = root.dataset.caret;
  const firstId = doc[0]?.id ?? "";
  const caret = rawCaret
    ? caretSchema.parse(JSON.parse(rawCaret))
    : { blockId: firstId, offset: 0 };

  const pos = { blockId: caret.blockId, offset: caret.offset };
  return { doc, sel: { anchor: pos, focus: pos } };
}

// ---------------------------------------------------------------------------
// Selection arithmetic helpers (generic, doc-free).
// ---------------------------------------------------------------------------

function isCollapsed(sel: EditorHistoryState["sel"]): boolean {
  return sel.anchor.blockId === sel.focus.blockId && sel.anchor.offset === sel.focus.offset;
}

/** The block id + offset the caret/anchor of an edit acts at. For a range on
 *  one block this is the ordered start; otherwise the focus position. */
function caretStart(sel: EditorHistoryState["sel"]): { blockId: string; offset: number } {
  if (sel.anchor.blockId === sel.focus.blockId) {
    return { blockId: sel.focus.blockId, offset: Math.min(sel.anchor.offset, sel.focus.offset) };
  }
  return { blockId: sel.focus.blockId, offset: sel.focus.offset };
}

/** Slice content to [start, end) preserving marks/href per run -- identical to
 *  the splice `removeText` itself computes (both go through `splitRuns`), so a
 *  `removeText` op built from this slice passes `removeText`'s exact
 *  `runsEqual` check instead of throwing. */
function sliceContent(
  content: string | InlineContent[] | undefined,
  start: number,
  end: number,
): InlineContent[] {
  const runs = normalizeRuns(content);
  const [, fromStart] = splitRuns(runs, start);
  const [middle] = splitRuns(fromStart, end - start);
  return middle;
}

/**
 * DOM-attribute config read for the score's aria/keymap projections --
 * label/labelledBy/disabled/readonly off `root`'s `data-*`, falling back to
 * an already-present `aria-label`/`aria-labelledby` (the Astro performance
 * sets these server-side; the Web Component's light-DOM markup can too).
 * Neither present projects no accessible name rather than inventing one --
 * the three decorators are the enforcement point for a required name
 * (`EditorLabelConfig`'s type-level union), not this runtime fallback, so an
 * unnamed root here means the decorator that rendered it didn't pass one.
 */
function parseEditorConfig(root: HTMLElement): EditorConfig {
  const labelledBy = root.dataset.labelledby ?? root.getAttribute("aria-labelledby") ?? undefined;
  const label = labelledBy
    ? undefined
    : (root.dataset.label ?? root.getAttribute("aria-label") ?? undefined);
  return {
    label,
    labelledBy,
    disabled: root.dataset.disabled === "true",
    readonly: root.dataset.readonly === "true",
  } as EditorConfig;
}

function mintBlockId(): string {
  if (typeof crypto !== "undefined" && typeof crypto.randomUUID === "function") {
    return crypto.randomUUID();
  }
  // Fallback for environments without the Web Crypto API. Block ids only need
  // to be unique within one document session, not cryptographically strong.
  return `block-${Date.now().toString(36)}-${Math.random().toString(36).slice(2)}`;
}

// ---------------------------------------------------------------------------
// translateBeforeInput -- the PURE, doc-free translation.
// ---------------------------------------------------------------------------

/**
 * Pure translation: a native `beforeinput` event + the current selection ->
 * the `EditorOp` it represents, or `null` when the inputType is unhandled.
 *
 * Only the ops fully determined WITHOUT the document are produced here --
 * `insertText`, whose payload is `event.data`. Deletes, merges, and paste are
 * NOT translated here and return `null`: a `removeText` op carries the removed
 * `InlineContent[]`, and `removeText()` throws unless that content
 * `runsEqual`s what is actually in the block, so it cannot be constructed
 * without reading the doc. `bindEditor` owns those doc-dependent ops; this
 * function is what vitest drives without a browser, `bindEditor` is what
 * Playwright drives with one. `text` is `InlineContent[]` end to end
 * (RULING-EDITOR-HISTORY AMENDMENT A), never a bare string.
 */
export function translateBeforeInput(
  event: Pick<InputEvent, "inputType" | "data">,
  sel: EditorHistoryState["sel"],
): EditorOp | null {
  switch (event.inputType) {
    case "insertText":
    case "insertReplacementText": {
      if (event.data == null) return null;
      const { blockId, offset } = caretStart(sel);
      return { kind: "insertText", blockId, offset, text: [{ text: event.data }] };
    }
    default:
      return null;
  }
}

// ---------------------------------------------------------------------------
// projectDocument -- model -> DOM, touching only changed blocks.
// ---------------------------------------------------------------------------

const MARK_TAG: Record<string, string> = {
  bold: "strong",
  italic: "em",
  code: "code",
  strikethrough: "s",
  link: "a",
};

/** Render one InlineContent run to a DOM node (text node, or nested mark
 *  wrappers around it). */
function renderRun(doc: Document, run: InlineContent): Node {
  let node: Node = doc.createTextNode(run.text);
  for (const mark of run.marks ?? []) {
    const tag = MARK_TAG[mark] ?? "span";
    const wrapper = doc.createElement(tag);
    if (mark === "link" && run.href !== undefined) wrapper.setAttribute("href", run.href);
    wrapper.appendChild(node);
    node = wrapper;
  }
  return node;
}

/** (Re)build a block element's content children from its model content. An
 *  empty block gets a single `<br>` -- as browsers render natively -- so it
 *  has height and accepts a caret; a `<br>` contributes nothing to
 *  `textContent`. */
function renderBlockContent(el: HTMLElement, block: BaseBlock): void {
  const doc = el.ownerDocument;
  el.replaceChildren();
  const runs = normalizeRuns(block.content);
  if (runs.length === 0 || totalTextLength(runs) === 0) {
    el.appendChild(doc.createElement("br"));
    return;
  }
  for (const run of runs) {
    if (run.text.length === 0) continue;
    el.appendChild(renderRun(doc, run));
  }
}

function createBlockElement(doc: Document, block: BaseBlock): HTMLElement {
  const el = doc.createElement("div");
  el.setAttribute("data-block-id", block.id);
  el.setAttribute("data-block-type", block.type);
  renderBlockContent(el, block);
  return el;
}

/**
 * Projects `doc` onto the contenteditable subtree under `root`, touching only
 * the DOM for blocks whose object identity changed since `previousDoc`
 * (NFR-EDITOR-004). Blocks reference-equal to their `previousDoc` entry are
 * left entirely untouched -- zero DOM writes for that subtree. Object-identity
 * preservation across an edit is `applyOp`'s job (FR-EDITOR-003's copy-on-write
 * convention); this function ACTS on that identity. Each block element carries
 * `data-block-id`; the bound root carries `data-part="root"`.
 */
export function projectDocument(
  root: HTMLElement,
  doc: EditorHistoryState["doc"],
  previousDoc: EditorHistoryState["doc"] | null,
): void {
  const ownerDoc = root.ownerDocument;

  const prevById = new Map<string, BaseBlock>();
  if (previousDoc) for (const block of previousDoc) prevById.set(block.id, block);

  const existing = new Map<string, HTMLElement>();
  for (const child of Array.from(root.children)) {
    const id = child.getAttribute("data-block-id");
    if (id !== null) existing.set(id, child as HTMLElement);
  }

  const docIds = new Set(doc.map((block) => block.id));
  for (const [id, el] of existing) {
    if (!docIds.has(id)) {
      el.remove();
      existing.delete(id);
    }
  }

  const desired: HTMLElement[] = [];
  for (const block of doc) {
    let el = existing.get(block.id);
    if (el === undefined) {
      el = createBlockElement(ownerDoc, block);
    } else if (prevById.get(block.id) !== block) {
      // Identity changed (or first sight of this block after a `null`
      // previousDoc): rebuild only this block's content in place.
      if (el.getAttribute("data-block-type") !== block.type) {
        el.setAttribute("data-block-type", block.type);
      }
      renderBlockContent(el, block);
    }
    desired.push(el);
  }

  // Order the children to match `doc`. Reusing an already-correctly-placed
  // element is a no-op; moving an untouched block's element is a mutation on
  // `root`, never on that block's own subtree, so it does not disturb the
  // reference-equality skip above.
  for (let i = 0; i < desired.length; i++) {
    const want = desired[i] as HTMLElement;
    const current = root.children[i] ?? null;
    if (current !== want) root.insertBefore(want, current);
  }
}

// ---------------------------------------------------------------------------
// Selection restore -- state.sel -> DOM Selection/Range.
// ---------------------------------------------------------------------------

function collectTextNodes(el: Node, out: Text[]): void {
  for (const child of Array.from(el.childNodes)) {
    if (child.nodeType === 3 /* TEXT_NODE */) out.push(child as Text);
    else collectTextNodes(child, out);
  }
}

/** Map a model (blockId, offset) to a concrete DOM (node, offset). */
function locatePosition(
  root: HTMLElement,
  pos: { blockId: string; offset: number },
): { node: Node; offset: number } | null {
  const blockEl = root.querySelector(`[data-block-id="${CSS.escape(pos.blockId)}"]`);
  if (blockEl === null) return null;

  const textNodes: Text[] = [];
  collectTextNodes(blockEl, textNodes);
  if (textNodes.length === 0) {
    // Empty block (renders a <br>): place the caret in the block element.
    return { node: blockEl, offset: 0 };
  }

  let remaining = pos.offset;
  for (const textNode of textNodes) {
    const len = textNode.data.length;
    if (remaining <= len) return { node: textNode, offset: remaining };
    remaining -= len;
  }
  const last = textNodes[textNodes.length - 1] as Text;
  return { node: last, offset: last.data.length };
}

/** Inverse of `locatePosition`: map a DOM (node, offset) boundary -- from a
 *  native `beforeinput` target range -- to the model text offset within
 *  `blockEl`. Measures with `Range.toString().length`, the same UTF-16
 *  code-unit count `sliceContent`/`removeText` use, so it agrees exactly with
 *  the model even when the boundary lands on an element (not just a text
 *  node) or inside a mark wrapper. Returns `null` when `node` isn't inside
 *  `blockEl` at all. */
function domOffsetInBlock(blockEl: Element, node: Node, offset: number): number | null {
  if (!blockEl.contains(node)) return null;
  try {
    const range = blockEl.ownerDocument.createRange();
    range.setStart(blockEl, 0);
    range.setEnd(node, offset);
    return range.toString().length;
  } catch {
    // A target range whose (node, offset) `Range.setStart`/`setEnd` rejects
    // (bad container/offset) must never escape into the `beforeinput`
    // listener -- per this file's own rule, typing must never break.
    // `deletionRange`'s caller already falls back to the UTF-16 arithmetic
    // for a `null` result.
    return null;
  }
}

function restoreSelection(root: HTMLElement, sel: EditorHistoryState["sel"]): void {
  const view = root.ownerDocument.defaultView;
  const selection = view?.getSelection?.() ?? null;
  if (selection === null) return;

  const focus = locatePosition(root, sel.focus);
  if (focus === null) return;
  const anchor = isCollapsed(sel) ? focus : locatePosition(root, sel.anchor);
  if (anchor === null) return;

  if (typeof selection.setBaseAndExtent === "function") {
    selection.setBaseAndExtent(anchor.node, anchor.offset, focus.node, focus.offset);
    return;
  }
  const range = root.ownerDocument.createRange();
  range.setStart(anchor.node, anchor.offset);
  range.setEnd(focus.node, focus.offset);
  selection.removeAllRanges();
  selection.addRange(range);
}

// ---------------------------------------------------------------------------
// bindEditor -- the controlled contenteditable lifecycle.
// ---------------------------------------------------------------------------

/**
 * Bind a controlled contenteditable to an editor history seeded from `root`'s
 * `data-initial-doc` / `data-caret`. Returns a teardown that removes every
 * listener and the history subscription.
 *
 * `history` is normally omitted: the WC and Astro decorators call
 * `bindEditor(root)` and get a fresh `createEditorHistory` built from `root`'s
 * seed data. React injects the SAME `EditorHistory` it reads via `useMemory`
 * (own `createEditorHistory` call in `editor.tsx`) so the declarative aria
 * projection and this binder's imperative DOM projection read one cell, not
 * two divergent ones -- still the ONE `bindEditor` export, no second binder,
 * no wrapper around it (Spec 05).
 *
 * FR-EDITOR-005 EXTENDS this same export with the aria projection (applied via
 * aria-manager, like every other DOM-native binder in this codebase) and the
 * undo/redo keymap wiring, gated on `history.canUndo`/`canRedo` -- there is no
 * `canDispatch` in this architecture.
 *
 * NOTE (deviates from RULING-EDITOR-HISTORY's pinned sketch
 * `bindEditor(root: HTMLElement): () => void`, recorded deliberately): the
 * second, optional `injectedHistory` parameter exists ONLY so React's own
 * `createEditorHistory` cell (read via `useMemory` in editor.tsx) and this
 * binder's imperative DOM projection stay the SAME cell rather than two
 * divergent ones. It is additive and optional -- every call site the pinned
 * signature covers (`bindEditor(root)`, the WC and Astro decorators) is
 * unaffected -- so it is not a second binder or a wrapper (Spec 05), just a
 * documented widening of the one export.
 */
export function bindEditor(root: HTMLElement, injectedHistory?: EditorHistory): () => void {
  root.setAttribute("data-part", "root");

  const history = injectedHistory ?? createEditorHistory(parseSeed(root));
  const { controls, memory } = history;

  const ids = { root: root.id } as PartIds<EditorPart>;

  // Config is read fresh from `root`'s data-* on every render, NOT closed
  // over once at bind time: disabled/readonly/label/labelledby can change
  // post-mount (a React re-render commits new data-* attributes to the DOM
  // declaratively, with no doc/sel change and therefore no memory
  // notification of its own). Re-reading here, plus the attribute observer
  // below forcing a render when only those data-* change, is what makes a
  // toggled `disabled` actually flip `contenteditable` and keeps this
  // binder's own aria write from reverting a label change made between
  // renders back to a stale value.
  let prevDoc: BaseBlock[] | null = null;
  function render(): void {
    const state = memory.get();
    const config = parseEditorConfig(root);
    root.setAttribute("contenteditable", config.disabled || config.readonly ? "false" : "true");
    projectDocument(root, state.doc, prevDoc);
    prevDoc = state.doc;
    restoreSelection(root, state.sel);

    const projection = editorAria(state, config, ids).root;
    if (projection) {
      for (const [name, value] of Object.entries(projection)) {
        updateAriaAttribute(root, name as never, value as never, { validate: false });
      }
    }
  }

  // Re-render on a data-* config change alone (no doc/sel change, so no
  // memory notification) -- e.g. a React prop toggling `disabled` with no
  // edit in between. Deliberately excludes aria-label/aria-labelledby:
  // render() itself writes those, and observing them would requeue a render
  // on every render.
  let attrObserver: MutationObserver | undefined;
  if (typeof MutationObserver !== "undefined") {
    attrObserver = new MutationObserver(() => render());
    attrObserver.observe(root, {
      attributes: true,
      attributeFilter: ["data-disabled", "data-readonly", "data-label", "data-labelledby"],
    });
  }

  // -- op construction for the doc-dependent inputs (deletes / structural) --

  /** A range selection spanning two OR MORE blocks (`anchor.blockId !==
   *  focus.blockId`, not collapsed): truncate the earlier block from its
   *  offset to its end, truncate the later block from its start to its
   *  offset, `delete` any block strictly between them, then `mergeNext` the
   *  (now-truncated) earlier block into the (now-truncated) later one --
   *  landing the caret at the earlier block's offset, same as a same-block
   *  range delete. Built entirely from the existing op vocabulary (no new op
   *  kind), the same compound-ops-for-one-user-action shape `splitOps` below
   *  already uses for a range-remove-then-split. */
  function deleteRangeAcrossBlocksOps(
    doc: EditorHistoryState["doc"],
    sel: EditorHistoryState["sel"],
  ): EditorOp[] {
    const anchorIndex = doc.findIndex((b) => b.id === sel.anchor.blockId);
    const focusIndex = doc.findIndex((b) => b.id === sel.focus.blockId);
    if (anchorIndex === -1 || focusIndex === -1) return [];
    const anchorFirst = anchorIndex <= focusIndex;
    const startIndex = anchorFirst ? anchorIndex : focusIndex;
    const endIndex = anchorFirst ? focusIndex : anchorIndex;
    const startOffset = anchorFirst ? sel.anchor.offset : sel.focus.offset;
    const endOffset = anchorFirst ? sel.focus.offset : sel.anchor.offset;

    const startBlock = doc[startIndex] as BaseBlock;
    const endBlock = doc[endIndex] as BaseBlock;
    const startTotal = totalTextLength(normalizeRuns(startBlock.content));

    const ops: EditorOp[] = [
      {
        kind: "removeText",
        blockId: startBlock.id,
        offset: startOffset,
        text: sliceContent(startBlock.content, startOffset, startTotal),
      },
      {
        kind: "removeText",
        blockId: endBlock.id,
        offset: 0,
        text: sliceContent(endBlock.content, 0, endOffset),
      },
    ];
    for (let i = startIndex + 1; i < endIndex; i++) {
      ops.push({ kind: "delete", blockId: (doc[i] as BaseBlock).id });
    }
    ops.push({ kind: "mergeNext", blockId: startBlock.id });
    return ops;
  }

  /** The [start, end) model-offset range a native `deleteContentBackward`/
   *  `Forward` actually removes, per the browser's own `beforeinput`
   *  `targetRanges` (`InputData.targetRanges`, from `getTargetRanges()`) --
   *  grapheme- and surrogate-pair-aware, unlike the UTF-16 code-unit `offset
   *  +/- 1` arithmetic `fallback` carries. Falls back to that arithmetic when
   *  no usable target range is available: no browser support, or a driver
   *  that never dispatches a real `beforeinput` (vitest/happy-dom -- see this
   *  file's test's own note that capture is Playwright-only). */
  function deletionRange(
    blockId: string,
    fallback: { start: number; end: number },
    targetRanges: readonly StaticRange[],
  ): { start: number; end: number } {
    const range = targetRanges[0];
    const blockEl = root.querySelector(`[data-block-id="${CSS.escape(blockId)}"]`);
    if (range === undefined || blockEl === null) return fallback;
    const start = domOffsetInBlock(blockEl, range.startContainer, range.startOffset);
    const end = domOffsetInBlock(blockEl, range.endContainer, range.endOffset);
    if (start === null || end === null) return fallback;
    return { start: Math.min(start, end), end: Math.max(start, end) };
  }

  /** A range selection on a SINGLE block (`anchor.blockId === focus.blockId`,
   *  not collapsed): remove [start, end) as one `removeText` op. Shared by
   *  `deleteBackwardOp` and `deleteForwardOp` -- a range delete removes the
   *  same span regardless of which key triggered it. */
  function sameBlockRangeRemoveOp(
    doc: EditorHistoryState["doc"],
    sel: EditorHistoryState["sel"],
  ): EditorOp[] {
    const start = Math.min(sel.anchor.offset, sel.focus.offset);
    const end = Math.max(sel.anchor.offset, sel.focus.offset);
    const block = doc.find((b) => b.id === sel.focus.blockId);
    return [
      {
        kind: "removeText",
        blockId: sel.focus.blockId,
        offset: start,
        text: sliceContent(block?.content, start, end),
      },
    ];
  }

  function deleteBackwardOp(
    state: EditorHistoryState,
    targetRanges: readonly StaticRange[],
  ): EditorOp[] {
    const { sel, doc } = state;
    if (!isCollapsed(sel)) {
      if (sel.anchor.blockId !== sel.focus.blockId) return deleteRangeAcrossBlocksOps(doc, sel);
      return sameBlockRangeRemoveOp(doc, sel);
    }
    const { blockId, offset } = sel.focus;
    if (offset === 0) {
      const index = doc.findIndex((b) => b.id === blockId);
      if (index <= 0) return []; // first block: nothing to merge into
      return [{ kind: "mergePrev", blockId }];
    }
    const block = doc.find((b) => b.id === blockId);
    const { start, end } = deletionRange(blockId, { start: offset - 1, end: offset }, targetRanges);
    return [
      {
        kind: "removeText",
        blockId,
        offset: start,
        text: sliceContent(block?.content, start, end),
      },
    ];
  }

  function deleteForwardOp(
    state: EditorHistoryState,
    targetRanges: readonly StaticRange[],
  ): EditorOp[] {
    const { sel, doc } = state;
    if (!isCollapsed(sel)) {
      if (sel.anchor.blockId !== sel.focus.blockId) return deleteRangeAcrossBlocksOps(doc, sel);
      return sameBlockRangeRemoveOp(doc, sel);
    }
    const { blockId, offset } = sel.focus;
    const block = doc.find((b) => b.id === blockId);
    const total = totalTextLength(normalizeRuns(block?.content));
    if (offset >= total) {
      const index = doc.findIndex((b) => b.id === blockId);
      if (index === -1 || index >= doc.length - 1) return []; // last block: nothing to merge
      return [{ kind: "mergeNext", blockId }];
    }
    const { start, end } = deletionRange(blockId, { start: offset, end: offset + 1 }, targetRanges);
    return [
      {
        kind: "removeText",
        blockId,
        offset: start,
        text: sliceContent(block?.content, start, end),
      },
    ];
  }

  /** Enter -> split. With a range selection, remove the range first (as a
   *  separate op) so the split lands at the collapsed point. */
  function splitOps(state: EditorHistoryState): EditorOp[] {
    const { sel } = state;
    const ops: EditorOp[] = [];
    let offset = sel.focus.offset;
    const blockId = sel.focus.blockId;
    if (!isCollapsed(sel) && sel.anchor.blockId === blockId) {
      const start = Math.min(sel.anchor.offset, sel.focus.offset);
      const end = Math.max(sel.anchor.offset, sel.focus.offset);
      const block = state.doc.find((b) => b.id === blockId);
      ops.push({
        kind: "removeText",
        blockId,
        offset: start,
        text: sliceContent(block?.content, start, end),
      });
      offset = start;
    }
    ops.push({ kind: "split", blockId, offset, newBlockId: mintBlockId() });
    return ops;
  }

  function applyOps(ops: EditorOp[]): void {
    try {
      for (const op of ops) controls.apply(op);
    } catch {
      // applyOp threw (unresolvable blockId / out-of-bounds offset): the native
      // edit was already prevented and the cell is untouched (commitEntry runs
      // the whole applyOp pass before any memory.set), so the next render
      // re-projects from the last valid state -- the DOM never diverges.
    }
  }

  // -- paste: read via createClipboard, own the insert --

  function pasteText(text: string): void {
    const lines = text.split("\n");
    const start = caretStart(memory.get().sel);
    controls.apply({
      kind: "insertText",
      blockId: start.blockId,
      offset: start.offset,
      text: [{ text: lines[0] ?? "" }],
    });
    for (let i = 1; i < lines.length; i++) {
      const afterSplit = memory.get().sel.focus;
      controls.apply({
        kind: "split",
        blockId: afterSplit.blockId,
        offset: afterSplit.offset,
        newBlockId: mintBlockId(),
      });
      const line = lines[i] ?? "";
      if (line.length > 0) {
        const at = memory.get().sel.focus;
        controls.apply({
          kind: "insertText",
          blockId: at.blockId,
          offset: 0,
          text: [{ text: line }],
        });
      }
    }
  }

  const clipboard = createClipboard({
    container: root,
    onPaste: (data) => {
      if (data.text) {
        try {
          pasteText(data.text);
        } catch {
          // See applyOps: the cell is left valid; the next render re-projects.
        }
      }
    },
  });
  // createClipboard reads the payload but does NOT preventDefault; the model
  // owns the edit, so the browser paste must be cancelled here.
  const onPasteRaw = (event: Event): void => {
    event.preventDefault();
  };
  root.addEventListener("paste", onPasteRaw);

  // -- capture: preventDefault + translate, IME via composition --

  let composing = false;
  let compositionAnchor = memory.get().sel;

  // Every `beforeinput` is preventDefault'd unconditionally so the browser
  // never mutates the contenteditable -- EXCEPT while an IME composition is
  // active, when the browser must render the in-progress composition (the
  // finalized string commits as one op on `compositionend`). Composition
  // inputTypes are otherwise not translated (see createInputHandler, which
  // suppresses insertText while composing).
  const onBeforeInputRaw = (event: Event): void => {
    if (composing || (event as InputEvent).isComposing) return;
    event.preventDefault();
  };
  root.addEventListener("beforeinput", onBeforeInputRaw);

  const inputHandler = createInputHandler({
    element: root,
    onBeforeInput: (data) => {
      const state = memory.get();
      switch (data.inputType) {
        case "insertText": {
          const op = translateBeforeInput(data, state.sel);
          if (op) applyOps([op]);
          return;
        }
        case "deleteContentBackward": {
          applyOps(deleteBackwardOp(state, data.targetRanges));
          return;
        }
        case "deleteContentForward": {
          applyOps(deleteForwardOp(state, data.targetRanges));
          return;
        }
        case "insertParagraph":
        case "insertLineBreak":
          applyOps(splitOps(state));
          return;
        case "insertFromPaste":
          // Handled by the clipboard read path above -- preventDefault only,
          // never apply here (double-apply guard).
          return;
        default:
          // Unrecognized/unsupported inputType (e.g. a native formatBold
          // shortcut): already preventDefault'd, treated as a no-op, never
          // throws -- typing must never break.
          return;
      }
    },
    onCompositionStart: () => {
      composing = true;
      compositionAnchor = memory.get().sel;
    },
    onCompositionEnd: (event) => {
      composing = false;
      const text = event.data;
      if (text) {
        const start = caretStart(compositionAnchor);
        applyOps([
          { kind: "insertText", blockId: start.blockId, offset: start.offset, text: [{ text }] },
        ]);
      }
      // Force a full re-projection: the browser rendered the composition into
      // the DOM directly, and a CANCELLED composition applies no op (no block
      // identity changes), so the reference-equality skip would otherwise leave
      // the browser's leftover composition DOM in place forever.
      prevDoc = null;
      render();
    },
  });

  // -- keydown: undo/redo (FR-EDITOR-005) --

  const onKeydown = (event: KeyboardEvent): void => {
    const action = editorKeymap(
      {
        key: event.key,
        shiftKey: event.shiftKey,
        ctrlKey: event.ctrlKey,
        altKey: event.altKey,
        metaKey: event.metaKey,
      },
      memory.get(),
      "root",
      parseEditorConfig(root),
    );
    if (!action) return;
    // preventDefault UNCONDITIONALLY: the model owns the edit, so the native
    // contenteditable undo/redo must never run even when the gate below turns
    // this into a no-op (an empty done/undone log).
    event.preventDefault();
    if (action === "undo" && history.canUndo) controls.undo();
    else if (action === "redo" && history.canRedo) controls.redo();
  };
  root.addEventListener("keydown", onKeydown);

  const unsubscribe = memory.subscribe(() => render());

  return () => {
    unsubscribe();
    attrObserver?.disconnect();
    inputHandler.cleanup();
    clipboard.cleanup();
    root.removeEventListener("paste", onPasteRaw);
    root.removeEventListener("beforeinput", onBeforeInputRaw);
    root.removeEventListener("keydown", onKeydown);
  };
}
