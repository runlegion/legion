/**
 * Internal content-splicing helpers shared by format.ts and text.ts.
 *
 * Not exported from ops/index.ts -- these are generic InlineContent[] array
 * utilities (slice-at-offset, merge-adjacent-identical-runs), not a
 * reimplementation of block-operations' structural logic or
 * inline-formatter's mark vocabulary/DOM controller.
 *
 * splitRuns/marksEqual/mergeRuns delegate to block-operations.ts's
 * splitInlineContent/inlineMarksEqual/mergeAdjacentRuns rather than carrying
 * a second copy -- structural.ts's merge inverse and text.ts's removeText
 * both depend on this canonicalization staying identical to
 * block-operations', so a second copy here would risk the two drifting
 * apart.
 */
import {
  inlineMarksEqual,
  mergeAdjacentRuns,
  splitInlineContent,
} from "@/lib/primitives/block-operations";
import type { InlineContent, InlineMark } from "@/lib/primitives/types";

/** Normalize string/undefined/array content into a run array for splicing. */
export function normalizeRuns(content: string | InlineContent[] | undefined): InlineContent[] {
  if (Array.isArray(content)) return content;
  if (content === undefined || content.length === 0) return [];
  return [{ text: content }];
}

export function totalTextLength(runs: InlineContent[]): number {
  return runs.reduce((sum, run) => sum + run.text.length, 0);
}

/** Split a run array at a character offset, preserving marks/href on each half. */
export const splitRuns: (
  runs: InlineContent[],
  offset: number,
) => [InlineContent[], InlineContent[]] = splitInlineContent;

export const marksEqual: (a: InlineMark[] | undefined, b: InlineMark[] | undefined) => boolean =
  inlineMarksEqual;

export function runsEqual(a: InlineContent[], b: InlineContent[]): boolean {
  if (a.length !== b.length) return false;
  return a.every((run, i) => {
    const other = b[i];
    return (
      other !== undefined &&
      run.text === other.text &&
      marksEqual(run.marks, other.marks) &&
      run.href === other.href
    );
  });
}

export function hasMark(run: InlineContent, mark: InlineMark): boolean {
  return !!run.marks?.includes(mark);
}

/** Add `mark` to a run, setting `href` when the mark is 'link'. */
export function withMark(
  run: InlineContent,
  mark: InlineMark,
  href: string | undefined,
): InlineContent {
  const marks = run.marks ? [...run.marks] : [];
  if (!marks.includes(mark)) marks.push(mark);
  const next: InlineContent = { text: run.text, marks };
  const nextHref = mark === "link" ? (href ?? run.href) : run.href;
  if (nextHref !== undefined) next.href = nextHref;
  return next;
}

/** Remove `mark` from a run, dropping `href` when the removed mark is 'link'. */
export function withoutMark(run: InlineContent, mark: InlineMark): InlineContent {
  const marks = (run.marks ?? []).filter((m) => m !== mark);
  const next: InlineContent = { text: run.text };
  if (marks.length > 0) next.marks = marks;
  if (run.href !== undefined && mark !== "link") next.href = run.href;
  return next;
}

/**
 * Collapse a run array back down to a plain string when none of its runs
 * carry a mark or href -- avoids gratuitously upgrading markless content to
 * InlineContent[] purely as a byproduct of splicing (which would break
 * exact round-trip equality for callers, such as mergePrev/mergeNext's
 * inverse, that must reconstruct an originally-plain-string block).
 */
export function collapseIfPlain(runs: InlineContent[]): string | InlineContent[] {
  const hasMarks = runs.some(
    (run) => (run.marks && run.marks.length > 0) || run.href !== undefined,
  );
  if (hasMarks) return runs;
  return runs.map((run) => run.text).join("");
}

/** Merge adjacent runs with identical mark sets and href; drop empty runs. */
export const mergeRuns: (runs: InlineContent[]) => InlineContent[] = mergeAdjacentRuns;
