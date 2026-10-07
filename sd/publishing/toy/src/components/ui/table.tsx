/**
 * Table component for displaying structured data in rows and columns
 *
 * @cognitive-load 3/10 - Familiar grid pattern; visual scanning is natural
 * @attention-economics Low attention cost: structured data is easy to scan
 * @trust-building Clear headers, consistent alignment, visible row separation
 * @accessibility Semantic table elements, proper scope attributes, keyboard navigable
 * @semantic-meaning Data presentation: lists, comparisons, structured information
 *
 * @usage-patterns
 * DO: Use for structured, comparable data
 * DO: Provide clear column headers
 * DO: Use consistent alignment (left for text, right for numbers)
 * DO: Support sorting and filtering for large datasets
 * DO: Consider sticky headers for long tables
 * NEVER: Use for layout purposes (use CSS Grid instead)
 * NEVER: Nest tables within tables
 * NEVER: Hide header row
 *
 * @example
 * ```tsx
 * <Table>
 *   <Table.Header>
 *     <Table.Row>
 *       <Table.Head>Name</Table.Head>
 *       <Table.Head>Status</Table.Head>
 *     </Table.Row>
 *   </Table.Header>
 *   <Table.Body>
 *     <Table.Row>
 *       <Table.Cell>John</Table.Cell>
 *       <Table.Cell>Active</Table.Cell>
 *     </Table.Row>
 *   </Table.Body>
 * </Table>
 * ```
 */
import * as React from 'react';
import classy from '@/lib/primitives/classy';
import { tableRowAttrs } from '@/components/ui/table.behavior';
import {
  tableBodyClasses,
  tableCaptionClasses,
  tableCellClasses,
  tableFooterClasses,
  tableHeadClasses,
  tableHeaderClasses,
  tableRootClasses,
  tableRowClasses,
  tableWrapperClasses,
} from '@/components/ui/table.classes';

export type TableProps = React.TableHTMLAttributes<HTMLTableElement>;

const TableRoot = React.forwardRef<HTMLTableElement, TableProps>(({ className, ...props }, ref) => (
  <div className={tableWrapperClasses}>
    <table ref={ref} data-part="root" className={classy(tableRootClasses, className)} {...props} />
  </div>
));

TableRoot.displayName = 'Table';

export type TableHeaderProps = React.HTMLAttributes<HTMLTableSectionElement>;

export const TableHeader = React.forwardRef<HTMLTableSectionElement, TableHeaderProps>(
  ({ className, ...props }, ref) => (
    <thead
      ref={ref}
      data-slot="table-header"
      className={classy(tableHeaderClasses, className)}
      {...props}
    />
  ),
);

TableHeader.displayName = 'TableHeader';

export type TableBodyProps = React.HTMLAttributes<HTMLTableSectionElement>;

export const TableBody = React.forwardRef<HTMLTableSectionElement, TableBodyProps>(
  ({ className, ...props }, ref) => (
    <tbody
      ref={ref}
      data-slot="table-body"
      className={classy(tableBodyClasses, className)}
      {...props}
    />
  ),
);

TableBody.displayName = 'TableBody';

export type TableFooterProps = React.HTMLAttributes<HTMLTableSectionElement>;

export const TableFooter = React.forwardRef<HTMLTableSectionElement, TableFooterProps>(
  ({ className, ...props }, ref) => (
    <tfoot
      ref={ref}
      data-slot="table-footer"
      className={classy(tableFooterClasses, className)}
      {...props}
    />
  ),
);

TableFooter.displayName = 'TableFooter';

export interface TableRowProps extends React.HTMLAttributes<HTMLTableRowElement> {
  /**
   * Selected state -- a consumer-set PROP, not an internal selection model.
   * When true, the row projects `aria-selected="true"` and the
   * `data-state="selected"` class hook; when absent, it projects neither.
   */
  selected?: boolean;
}

export const TableRow = React.forwardRef<HTMLTableRowElement, TableRowProps>(
  ({ selected, className, ...props }, ref) => (
    <tr
      ref={ref}
      data-part="row"
      className={classy(tableRowClasses, className)}
      {...tableRowAttrs(selected)}
      {...props}
    />
  ),
);

TableRow.displayName = 'TableRow';

export type TableHeadProps = React.ThHTMLAttributes<HTMLTableCellElement>;

export const TableHead = React.forwardRef<HTMLTableCellElement, TableHeadProps>(
  ({ className, ...props }, ref) => (
    <th
      ref={ref}
      data-slot="table-head"
      className={classy(tableHeadClasses, className)}
      {...props}
    />
  ),
);

TableHead.displayName = 'TableHead';

export type TableCellProps = React.TdHTMLAttributes<HTMLTableCellElement>;

export const TableCell = React.forwardRef<HTMLTableCellElement, TableCellProps>(
  ({ className, ...props }, ref) => (
    <td
      ref={ref}
      data-slot="table-cell"
      className={classy(tableCellClasses, className)}
      {...props}
    />
  ),
);

TableCell.displayName = 'TableCell';

export type TableCaptionProps = React.HTMLAttributes<HTMLTableCaptionElement>;

export const TableCaption = React.forwardRef<HTMLTableCaptionElement, TableCaptionProps>(
  ({ className, ...props }, ref) => (
    <caption
      ref={ref}
      data-slot="table-caption"
      className={classy(tableCaptionClasses, className)}
      {...props}
    />
  ),
);

TableCaption.displayName = 'TableCaption';

/** The shadcn v4 compound surface: Table + namespaced sub-components. */
export const Table = Object.assign(TableRoot, {
  Header: TableHeader,
  Body: TableBody,
  Footer: TableFooter,
  Row: TableRow,
  Head: TableHead,
  Cell: TableCell,
  Caption: TableCaption,
});

export default Table;
