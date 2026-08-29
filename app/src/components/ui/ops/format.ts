/**
 * Format ops (FR-EDITOR-003) -- apply/remove inline marks over a block's
 * InlineContent[], invertible by construction.
 *
 * Reuses InlineMark (primitives/types.ts) as the closed mark vocabulary, the
 * same set inline-formatter.ts's BOLD/ITALIC/CODE/STRIKETHROUGH/LINK format
 * definitions name. Operates on a block's InlineContent[] directly -- NOT via
 * inline-formatter's DOM controller (createInlineFormatter/applyFormat/
 * removeFormat), which reads window.getSelection() and a live contenteditable
 * container and has no place in a pure function over a data document.
 */
import type { BaseBlock } from "@/lib/primitives/types";
import {
  hasMark,
  mergeRuns,
  normalizeRuns,
  splitRuns,
  totalTextLength,
  withMark,
  withoutMark,
} from "./content";
import type { EditorOp, FormatOp, OpResult } from "./types";

function findBlockIndex(blocks: BaseBlock[], blockId: string, opName: string): number {
  const index = blocks.findIndex((b) => b.id === blockId);
  if (index === -1) throw new Error(`${opName}: block "${blockId}" not found`);
  return index;
}

function validateRange(
  start: number,
  end: number,
  total: number,
  blockId: string,
  opName: string,
): void {
  if (start < 0 || end > total || start > end) {
    throw new Error(
      `${opName}: range [${start}, ${end}) out of bounds for block "${blockId}" (length ${total})`,
    );
  }
}

export function applyMark(
  blocks: BaseBlock[],
  op: Extract<FormatOp, { kind: "applyMark" }>,
): OpResult {
  const { blockId, start, end, mark, href } = op;
  const index = findBlockIndex(blocks, blockId, "applyMark");
  const block = blocks[index] as BaseBlock;
  const runs = normalizeRuns(block.content);
  const total = totalTextLength(runs);
  validateRange(start, end, total, blockId, "applyMark");

  if (start === end) return { blocks, inverse: [] };

  const [before, fromStart] = splitRuns(runs, start);
  const [middle, after] = splitRuns(fromStart, end - start);

  // Capture the maximal contiguous sub-ranges of [start, end) that did NOT
  // already carry `mark` -- these are exactly what removeMark must undo.
  const gaps: Array<{ start: number; end: number }> = [];
  let pos = start;
  let gapStart: number | null = null;
  for (const run of middle) {
    if (!hasMark(run, mark)) {
      if (gapStart === null) gapStart = pos;
    } else if (gapStart !== null) {
      gaps.push({ start: gapStart, end: pos });
      gapStart = null;
    }
    pos += run.text.length;
  }
  if (gapStart !== null) gaps.push({ start: gapStart, end: pos });

  // For the 'link' mark specifically, a run that already carries the mark
  // but with a DIFFERENT href has that href silently overwritten by
  // withMark below. Capture those sub-ranges (grouped by their prior href)
  // so the inverse can restore the original href instead of merely
  // stripping the mark (which is all `gaps` above accounts for -- gaps only
  // covers ranges that did NOT already have the mark).
  const hrefChanges: Array<{ start: number; end: number; href: string | undefined }> = [];
  if (mark === "link" && href !== undefined) {
    pos = start;
    let segStart: number | null = null;
    let segHref: string | undefined;
    for (const run of middle) {
      const changes = hasMark(run, mark) && run.href !== href;
      if (changes && segStart !== null && run.href === segHref) {
        // continue the current segment
      } else {
        if (segStart !== null) {
          hrefChanges.push({ start: segStart, end: pos, href: segHref });
          segStart = null;
          segHref = undefined;
        }
        if (changes) {
          segStart = pos;
          segHref = run.href;
        }
      }
      pos += run.text.length;
    }
    if (segStart !== null) {
      hrefChanges.push({ start: segStart, end: pos, href: segHref });
    }
  }

  const newMiddle = middle.map((run) => withMark(run, mark, href));
  const content = mergeRuns([...before, ...newMiddle, ...after]);

  const newBlocks = [...blocks];
  newBlocks[index] = { ...block, content };

  const inverse: EditorOp[] = [
    ...gaps.map((gap): EditorOp => ({
      kind: "removeMark",
      blockId,
      start: gap.start,
      end: gap.end,
      mark,
    })),
    // Two-op pair per href change: strip the mark (clearing the overwritten
    // href) then reapply with the original href -- a single applyMark op
    // can't express "restore to no href" since an omitted `href` field
    // means "keep the run's current href" (see withMark), not "clear it".
    ...hrefChanges.flatMap((seg): EditorOp[] => [
      { kind: "removeMark", blockId, start: seg.start, end: seg.end, mark },
      {
        kind: "applyMark",
        blockId,
        start: seg.start,
        end: seg.end,
        mark,
        ...(seg.href !== undefined ? { href: seg.href } : {}),
      },
    ]),
  ];

  return { blocks: newBlocks, inverse };
}

export function removeMark(
  blocks: BaseBlock[],
  op: Extract<FormatOp, { kind: "removeMark" }>,
): OpResult {
  const { blockId, start, end, mark } = op;
  const index = findBlockIndex(blocks, blockId, "removeMark");
  const block = blocks[index] as BaseBlock;
  const runs = normalizeRuns(block.content);
  const total = totalTextLength(runs);
  validateRange(start, end, total, blockId, "removeMark");

  if (start === end) return { blocks, inverse: [] };

  const [before, fromStart] = splitRuns(runs, start);
  const [middle, after] = splitRuns(fromStart, end - start);

  // Capture the maximal contiguous sub-ranges of [start, end) that DID carry
  // `mark` (with its href, for the 'link' mark) -- what applyMark must redo.
  // A run boundary between two marked runs with DIFFERENT hrefs also closes
  // a segment (not just a run lacking the mark) -- otherwise two adjacent
  // links with different hrefs collapse into one segment carrying only the
  // first href, and the second href is lost on undo.
  const segments: Array<{ start: number; end: number; href?: string }> = [];
  let pos = start;
  let segStart: number | null = null;
  let segHref: string | undefined;
  for (const run of middle) {
    const marked = hasMark(run, mark);
    if (marked && segStart !== null && run.href === segHref) {
      // continue the current segment
    } else {
      if (segStart !== null) {
        segments.push({
          start: segStart,
          end: pos,
          ...(segHref !== undefined ? { href: segHref } : {}),
        });
        segStart = null;
        segHref = undefined;
      }
      if (marked) {
        segStart = pos;
        segHref = run.href;
      }
    }
    pos += run.text.length;
  }
  if (segStart !== null) {
    segments.push({
      start: segStart,
      end: pos,
      ...(segHref !== undefined ? { href: segHref } : {}),
    });
  }

  const newMiddle = middle.map((run) => withoutMark(run, mark));
  const content = mergeRuns([...before, ...newMiddle, ...after]);

  const newBlocks = [...blocks];
  newBlocks[index] = { ...block, content };

  const inverse: EditorOp[] = segments.map((seg) => ({
    kind: "applyMark",
    blockId,
    start: seg.start,
    end: seg.end,
    mark,
    ...(seg.href !== undefined ? { href: seg.href } : {}),
  }));

  return { blocks: newBlocks, inverse };
}
