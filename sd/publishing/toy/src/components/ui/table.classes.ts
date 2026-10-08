/**
 * Table view: class strings, no logic. Shared by every performance
 * (table.tsx, table.astro) so one score wears one decoration across
 * frameworks. Ported from the oracle; the sub-
 * part classes are config-independent literals, so the performances import
 * them directly (no context/provider needed for a flat static).
 *
 * All colour and typography come through semantic role tokens
 * (`text-body-small ts-body-small`, `text-label-medium ts-label-medium`, `text-muted-foreground`, `bg-muted`,
 * `border-b`/`border-t`) -- no raw colour or arbitrary values. The one delta
 * from the oracle is motion: its literal `duration-150` is replaced by the
 * generics the matrix assigns, `duration-fast ease-standard` (see the row
 * on `tableRowClasses`). Leaving the timing off entirely, as this file did
 * before, took Tailwind's stock default rather than the token -- which is the
 * same defect as the literal, one step quieter.
 */

/** The `<table>` root: full width, bottom-anchored caption, small body text. */
export const tableRootClasses = 'w-full caption-bottom text-body-small ts-body-small';

/** The overflow wrapper around the table -- horizontal scroll for wide data. */
export const tableWrapperClasses = 'relative w-full overflow-auto';

/** `<thead>`: a bottom border under the header row. */
export const tableHeaderClasses = '[&_tr]:border-b';

/** `<tbody>`: the last row drops its border so it does not double the footer. */
export const tableBodyClasses = '[&_tr:last-child]:border-0';

/** `<tfoot>`: a top border, muted surface, label typography. */
export const tableFooterClasses =
  'border-t bg-muted/50 text-label-medium ts-label-medium [&>tr]:last:border-b-0';

/**
 * `<tr>`: a bottom border, a colour transition on hover, and the selected
 * hook. `data-[state=selected]:bg-muted` is the class side of the
 * `tableRowAttrs` projection -- a selected row (data-state="selected") tints
 * to the muted surface.
 *
 * NO component-level reduced-motion escape. The generated `duration-*` and
 * `delay-*` utilities zero themselves under `prefers-reduced-motion` (the
 * exporter's `REDUCED_MOTION_ZEROED` set), so reduced motion is the token
 * sheet's responsibility and never a component-level media query. The
 * `motion-reduce:transition-none` this drops was redundant with that law.
 *
 * TWO ROWS, ONE TRANSITION. `table / row / hover` (provenance `baseline`) and
 * `table / row / selected <-> unselected` (provenance `proposed`) both assign
 * tier `fast` and curve role `standard` to the same colour change
 * (`background, text, border`) on the same part. One `duration-fast
 * ease-standard` satisfies both, which is the exporter's own rule --
 * deduplicated by the motion, not by the moment. Two identical strings here
 * would be two names for one motion. Neither moment is a keyframe: the row
 * stays mounted through both.
 */
export const tableRowClasses =
  'border-b transition-colors duration-fast ease-standard hover:bg-muted/50 data-[state=selected]:bg-muted';

/**
 * `<th>`: header cell -- fixed height, left-aligned, muted label typography.
 * The `[&:has([role=checkbox])]` and `[&>[role=checkbox]]` selectors align a
 * selection checkbox flush in the cell (shadcn v4 surface).
 */
export const tableHeadClasses =
  'h-10 px-2 text-left align-middle text-label-medium ts-label-medium text-muted-foreground [&:has([role=checkbox])]:pr-0 [&>[role=checkbox]]:translate-y-0.5';

/** `<td>`: data cell -- padded, middle-aligned, with the same checkbox flush. */
export const tableCellClasses =
  'p-2 align-middle [&:has([role=checkbox])]:pr-0 [&>[role=checkbox]]:translate-y-0.5';

/** `<caption>`: muted small text, anchored to the bottom by the root. */
export const tableCaptionClasses = 'text-body-small ts-body-small text-muted-foreground';
