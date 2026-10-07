import type { AriaAttrs, BehaviorSpec } from '@/lib/contract';

/**
 * Table: a semantic data table. The composition archetype -- like Card and
 * Container, a static score with NO state, NO actions, NO keymap, NO effects.
 * A `<table>`/`<thead>`/`<tbody>`/`<tr>`/`<th>`/`<td>` tree carries its own
 * native semantics (role=table/rowgroup/row/columnheader/cell), so the score
 * projects nothing onto the root; the surface is pure decoration.
 *
 * Because the root projection is empty and there is nothing to react to, Table
 * needs NO client at all: there is no `bindTable`, the React performance uses
 * no `useBehavior`/`useMemory`, and the Astro performance ships no `<script>`.
 * This is the same pure-static finding Card records -- a static's framework
 * files are markup + classes + slots, nothing more.
 *
 * The one contract beyond the empty root projection is the SELECTED ROW. A row
 * can carry a selected state, but that state is a PROP the consumer sets, not
 * an internal state machine the score runs (there is no selection model, no
 * dispatch, no keymap). The projection of that prop -- `aria-selected` plus the
 * `data-state` class hook -- is `tableRowAttrs`, a per-instance function each
 * performance applies at render, exactly as Grid's items declare their
 * priority through `gridItemAttrs`. `row` is declared `many`/`optional` so the
 * conformance harness can audit the per-instance projection
 * (`assertInstanceContractFulfillment`), and it carries NO `role` in its decl
 * because a native `<tr>` already maps to role=row with no attribute.
 *
 * The composition family (TableHeader, TableBody, TableFooter, TableHead,
 * TableCell, TableCaption) carries no behaviour of its own -- those are plain
 * framework wrappers over literal class strings, composed by the consumer
 * inside a Table. Only `root` and `row` are declared parts, because they are
 * the only nodes with a contract to audit (root: the empty projection; row:
 * the selected projection).
 */

export type TableConfig = Record<never, never>;
export type TableState = Record<never, never>;
export type TableActions = Record<never, never>;
export type TablePart = 'root' | 'row';

export const table: BehaviorSpec<TableConfig, TableState, TableActions, TablePart> = {
  name: 'table',
  parts: {
    root: {},
    // A native <tr> is role=row implicitly, so the decl carries NO role: the
    // conformance harness would otherwise assert an explicit role attribute
    // the semantic element never needs. The per-instance selected projection
    // is audited through tableRowAttrs, not spec.aria.
    row: { many: true, optional: true },
  },
  initialState: () => ({}),
  actions: {},
  canDispatch: () => true,
  // The table's semantics are native to the element tree; the score projects
  // nothing onto the root, and the harness asserts the empty contract.
  aria: () => ({ root: {} }),
  keymap: () => null,
};

/**
 * Per-instance projection for a table row: the row DECLARES whether it is
 * selected; the consumer sets it as a prop, never a state machine. A selected
 * row carries `aria-selected="true"` (a spec-supported property of role=row)
 * and the `data-state="selected"` hook the row decoration keys off
 * (`data-[state=selected]:bg-muted`). An unselected row carries NEITHER --
 * `aria-selected="false"` on every row is noise, so absence is the default and
 * only the selected row is announced.
 */
export function tableRowAttrs(selected: boolean | undefined): AriaAttrs {
  return {
    'aria-selected': selected ? 'true' : undefined,
    'data-state': selected ? 'selected' : undefined,
  };
}
