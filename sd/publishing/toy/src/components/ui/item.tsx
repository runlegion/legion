/**
 * Generic list item component for menus, lists, and selection interfaces
 *
 * @cognitive-load 3/10 - Familiar list pattern with clear visual states and predictable behavior
 * @attention-economics Secondary selection: Selected state draws focus, disabled reduces prominence. Icon slot provides visual anchoring for quick scanning.
 * @trust-building Consistent hover/focus/selected states build predictable interaction patterns. Clear disabled state prevents user confusion.
 * @accessibility Proper aria-selected for selection, aria-disabled for disabled state, keyboard navigation support, focus-visible for keyboard users
 * @semantic-meaning Building block for: menu items (navigation/actions), list items (content/data), option items (selection interfaces)
 *
 * @usage-patterns
 * DO: Use as building block for menu items, list items, selection options
 * DO: Include icons on the left for quick visual scanning
 * DO: Add description for secondary information or context
 * DO: Use selected state for current/active items in navigation
 * DO: Use disabled state for unavailable options with clear visual feedback
 * NEVER: Use for primary actions (use Button instead)
 * NEVER: Nest interactive elements within Item
 * NEVER: Use Item without a container (list, menu, etc.)
 *
 * @example
 * ```tsx
 * // Basic list item
 * <Item>Settings</Item>
 *
 * // With icon and description
 * <Item
 *   icon={<UserIcon className="h-4 w-4" />}
 *   description="Manage your account settings"
 * >
 *   Profile
 * </Item>
 *
 * // Selected state for navigation
 * <Item selected icon={<HomeIcon className="h-4 w-4" />}>
 *   Dashboard
 * </Item>
 *
 * // Disabled option
 * <Item disabled icon={<LockIcon className="h-4 w-4" />}>
 *   Admin Panel
 * </Item>
 *
 * // Interactive item with handler
 * <Item onClick={handleSelect} icon={<SettingsIcon className="h-4 w-4" />}>
 *   Settings
 * </Item>
 * ```
 */
import * as React from 'react';
import classy from '@/lib/primitives/classy';
import { item, type ItemConfig, type ItemSize } from '@/components/ui/item.behavior';
import {
  itemClasses,
  itemContentClasses,
  itemDescriptionClasses,
  itemIconClasses,
  itemLabelClasses,
} from '@/components/ui/item.classes';

export interface ItemProps extends React.HTMLAttributes<HTMLDivElement> {
  /** Icon element displayed before the item content. */
  icon?: React.ReactNode;
  /** Secondary description text displayed below the main content. */
  description?: React.ReactNode;
  /** Whether the item is in a selected/active state. */
  selected?: boolean;
  /** Whether the item is disabled and non-interactive. */
  disabled?: boolean;
  /** Visual size variant. */
  size?: ItemSize;
}

export const Item = React.forwardRef<HTMLDivElement, ItemProps>(
  (
    {
      icon,
      description,
      selected = false,
      disabled = false,
      size = 'default',
      className,
      children,
      ...props
    },
    ref,
  ) => {
    const config: ItemConfig = { size, selected, disabled };
    const classes = itemClasses(config, {});
    // The score decides the option semantics; the performance only applies
    // them. Spread before `props` so a consumer edge-case can still override,
    // matching the Grid performance's order.
    const aria = item.aria({}, config, { root: '' }).root ?? {};

    return (
      <div
        ref={ref}
        data-part="root"
        className={classy(classes.root, className)}
        {...aria}
        {...props}
      >
        {icon && (
          <div className={itemIconClasses} aria-hidden="true">
            {icon}
          </div>
        )}
        <div className={itemContentClasses}>
          <div className={itemLabelClasses}>{children}</div>
          {description && <div className={itemDescriptionClasses}>{description}</div>}
        </div>
      </div>
    );
  },
);

Item.displayName = 'Item';

export default Item;
