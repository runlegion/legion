/**
 * Empty state display for when there's no content to show
 *
 * @cognitive-load 2/10 - Simple informational display with clear next steps
 * @attention-economics Supportive element: Fills void without demanding attention, guides to action
 * @trust-building Honest communication about empty state builds trust; actionable guidance reduces frustration
 * @accessibility Clear heading hierarchy; actionable elements are keyboard accessible
 * @semantic-meaning State communication: empty search results, no items yet, cleared list, filtered to nothing
 *
 * @usage-patterns
 * DO: Provide actionable next steps when possible
 * DO: Explain why the state is empty (no results, no items yet, etc.)
 * DO: Use appropriate illustrations to soften the empty state
 * DO: Match tone to context (playful for personal apps, professional for business)
 * NEVER: Leave empty states blank
 * NEVER: Use generic "No data" without context
 * NEVER: Make users feel like something is broken
 *
 * @example
 * ```tsx
 * // Empty search results
 * <Empty>
 *   <EmptyIcon>
 *     <SearchIcon />
 *   </EmptyIcon>
 *   <EmptyTitle>No results found</EmptyTitle>
 *   <EmptyDescription>
 *     Try adjusting your search terms or filters.
 *   </EmptyDescription>
 *   <EmptyAction>
 *     <Button variant="outline" onClick={clearFilters}>
 *       Clear filters
 *     </Button>
 *   </EmptyAction>
 * </Empty>
 *
 * // Empty list (first time)
 * <Empty>
 *   <EmptyIcon>
 *     <FolderIcon />
 *   </EmptyIcon>
 *   <EmptyTitle>No projects yet</EmptyTitle>
 *   <EmptyDescription>
 *     Create your first project to get started.
 *   </EmptyDescription>
 *   <EmptyAction>
 *     <Button>Create project</Button>
 *   </EmptyAction>
 * </Empty>
 *
 * // Informational only (no action)
 * <Empty>
 *   <EmptyIcon>
 *     <InboxIcon />
 *   </EmptyIcon>
 *   <EmptyTitle>All caught up!</EmptyTitle>
 *   <EmptyDescription>
 *     No new notifications.
 *   </EmptyDescription>
 * </Empty>
 * ```
 */
import * as React from 'react';
import classy from '@/lib/primitives/classy';
import {
  emptyActionClasses,
  emptyClasses,
  emptyDescriptionClasses,
  emptyIconClasses,
  emptyTitleClasses,
} from '@/components/ui/empty.classes';

export type EmptyProps = React.HTMLAttributes<HTMLDivElement>;

export const Empty = React.forwardRef<HTMLDivElement, EmptyProps>(
  ({ className, children, ...props }, ref) => {
    const classes = emptyClasses({}, {});
    return (
      <div
        ref={ref}
        data-part="root"
        className={classy(classes.root, className) || undefined}
        {...props}
      >
        {children}
      </div>
    );
  },
);

Empty.displayName = 'Empty';

export type EmptyIconProps = React.HTMLAttributes<HTMLDivElement>;

/** The illustrative icon slot -- muted, sized for the placeholder. Plain
 *  composition over a literal class string, no data-part. */
export const EmptyIcon = React.forwardRef<HTMLDivElement, EmptyIconProps>(
  ({ className, ...props }, ref) => (
    <div
      ref={ref}
      data-slot="empty-icon"
      className={classy(emptyIconClasses, className)}
      {...props}
    />
  ),
);

EmptyIcon.displayName = 'EmptyIcon';

export interface EmptyTitleProps extends React.HTMLAttributes<HTMLHeadingElement> {
  as?: 'h1' | 'h2' | 'h3' | 'h4' | 'h5' | 'h6';
}

/**
 * Renders a raw heading (default h3, `as` = h1..h6) via createElement because
 * Typography's H1-H6 do not exist yet in the new tree (matrix: typography,
 * pending) -- the same raw-heading disposition Card and Alert record. `as`
 * places the heading at the correct outline level for the surrounding page;
 * the oracle hard-coded `h3`, so the default keeps byte-identical output.
 */
export const EmptyTitle = React.forwardRef<HTMLHeadingElement, EmptyTitleProps>(
  ({ as: Element = 'h3', className, children, ...props }, ref) =>
    React.createElement(
      Element,
      {
        ref,
        'data-slot': 'empty-title',
        className: classy(emptyTitleClasses, className),
        ...props,
      },
      children,
    ),
);

EmptyTitle.displayName = 'EmptyTitle';

export type EmptyDescriptionProps = React.HTMLAttributes<HTMLParagraphElement>;

/** Raw paragraph via createElement, same Typography-pending disposition as the
 *  title -- plain composition over a literal class string. */
export const EmptyDescription = React.forwardRef<HTMLParagraphElement, EmptyDescriptionProps>(
  ({ className, children, ...props }, ref) =>
    React.createElement(
      'p',
      {
        ref,
        'data-slot': 'empty-description',
        className: classy(emptyDescriptionClasses, className),
        ...props,
      },
      children,
    ),
);

EmptyDescription.displayName = 'EmptyDescription';

export type EmptyActionProps = React.HTMLAttributes<HTMLDivElement>;

/** Trailing action slot -- holds a real, keyboard-reachable control (a Button
 *  or link) that offers the next step. Plain composition, no data-part. */
export const EmptyAction = React.forwardRef<HTMLDivElement, EmptyActionProps>(
  ({ className, ...props }, ref) => (
    <div
      ref={ref}
      data-slot="empty-action"
      className={classy(emptyActionClasses, className)}
      {...props}
    />
  ),
);

EmptyAction.displayName = 'EmptyAction';

export default Empty;
