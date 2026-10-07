/**
 * Roving Focus primitive
 * Implements roving tabindex pattern for keyboard navigation
 * in composite widgets (menus, radio groups, toolbars, tabs)
 *
 * WCAG Compliance:
 * - 2.1.1 Keyboard (Level A): Arrow key navigation available
 * - 2.4.3 Focus Order (Level A): Logical focus order with Tab
 * - 4.1.2 Name, Role, Value (Level A): Current item communicated to AT
 *
 * @see https://www.w3.org/WAI/ARIA/apg/patterns/menubar/
 * @see https://www.w3.org/WAI/ARIA/apg/patterns/radio/
 */

import type { CleanupFunction, Direction, NavigationCallback, Orientation } from '@/lib/primitives/types';

export interface RovingFocusOptions {
  /**
   * Whether roving focus is enabled
   * @default true
   */
  enabled?: boolean;

  /**
   * Navigation orientation
   * - 'horizontal': Arrow Left/Right navigate
   * - 'vertical': Arrow Up/Down navigate
   * - 'both': All arrow keys navigate
   * @default 'horizontal'
   */
  orientation?: Orientation;

  /**
   * Two-dimensional grid navigation. When set, arrow keys rove a fixed-column
   * grid instead of a 1D list: ArrowLeft/Right move by 1, ArrowUp/Down move by
   * a full row (`columns`), Home/End jump to the first/last item. Movement
   * clamps at the edges (no wrap). Reuses the same [data-roving-item] tabstop,
   * focus tracking, and disabled filtering as the 1D path; the `orientation`,
   * `loop`, and `dir` options do not apply in this mode. This is the ARIA grid
   * pattern's keyboard contract (role="grid" with a fixed column count).
   */
  columns?: number;

  /**
   * Whether to wrap navigation at ends
   * @default true
   */
  loop?: boolean;

  /**
   * Text direction for RTL support
   * @default 'ltr'
   */
  dir?: Direction;

  /**
   * Initial focused item index
   * @default 0
   */
  currentIndex?: number;

  /**
   * Callback when navigation occurs
   */
  onNavigate?: NavigationCallback;

  /**
   * Prevent default on handled keys
   * @default true
   */
  preventDefault?: boolean;
}

/**
 * Get focusable items within container
 * Excludes disabled and hidden elements
 */
function getFocusableItems(container: HTMLElement): HTMLElement[] {
  const items = Array.from(
    container.querySelectorAll<HTMLElement>(
      '[data-roving-item], [role="menuitem"], [role="option"], [role="radio"], [role="tab"]',
    ),
  );

  return items.filter((item) => {
    // Skip disabled items (check all common disabled patterns)
    if (
      item.hasAttribute('disabled') ||
      item.hasAttribute('data-disabled') ||
      item.getAttribute('aria-disabled') === 'true'
    ) {
      return false;
    }
    // Skip hidden items
    if (item.hasAttribute('hidden') || item.getAttribute('aria-hidden') === 'true') {
      return false;
    }
    // Skip invisible items
    const style = getComputedStyle(item);
    if (style.display === 'none' || style.visibility === 'hidden') {
      return false;
    }
    return true;
  });
}

/**
 * Update tabindex on items (roving tabindex pattern)
 * Only one item should have tabindex="0" at a time
 */
function updateTabindex(items: HTMLElement[], currentIndex: number): void {
  for (let i = 0; i < items.length; i++) {
    const item = items[i];
    if (item) {
      item.setAttribute('tabindex', i === currentIndex ? '0' : '-1');
    }
  }
}

/**
 * Calculate next index based on direction and options
 */
function getNextIndex(
  currentIndex: number,
  direction: 1 | -1,
  itemCount: number,
  loop: boolean,
): number {
  const nextIndex = currentIndex + direction;

  if (loop) {
    if (nextIndex < 0) return itemCount - 1;
    if (nextIndex >= itemCount) return 0;
    return nextIndex;
  }

  return Math.max(0, Math.min(nextIndex, itemCount - 1));
}

/**
 * Check if key should navigate based on orientation
 */
function shouldNavigate(
  key: string,
  orientation: Orientation,
  dir: Direction,
): { navigate: boolean; direction: 1 | -1 } {
  const isRTL = dir === 'rtl';

  // Map keys to navigation direction
  const keyMap: Record<string, { orientations: Orientation[]; direction: 1 | -1 }> = {
    ArrowRight: {
      orientations: ['horizontal', 'both'],
      direction: isRTL ? -1 : 1,
    },
    ArrowLeft: {
      orientations: ['horizontal', 'both'],
      direction: isRTL ? 1 : -1,
    },
    ArrowDown: {
      orientations: ['vertical', 'both'],
      direction: 1,
    },
    ArrowUp: {
      orientations: ['vertical', 'both'],
      direction: -1,
    },
  };

  const mapping = keyMap[key];
  if (!mapping) {
    return { navigate: false, direction: 1 };
  }

  const matchesOrientation = mapping.orientations.includes(orientation) || orientation === 'both';

  return {
    navigate: matchesOrientation,
    direction: mapping.direction,
  };
}

/**
 * Grid navigation delta for a key in 2D mode: ArrowLeft/Right step one column,
 * ArrowUp/Down step a full row (`columns`). Returns null for keys that do not
 * rove the grid, so the caller leaves them to Home/End or the browser.
 */
function gridDelta(key: string, columns: number): number | null {
  switch (key) {
    case 'ArrowRight':
      return 1;
    case 'ArrowLeft':
      return -1;
    case 'ArrowDown':
      return columns;
    case 'ArrowUp':
      return -columns;
    default:
      return null;
  }
}

/**
 * Create roving focus behavior for a container
 * Returns cleanup function to remove event listeners
 *
 * @example
 * ```typescript
 * const cleanup = createRovingFocus(menuElement, {
 *   orientation: 'vertical',
 *   loop: true,
 *   onNavigate: (element, index) => {
 *     // Update active state in your framework
 *   }
 * });
 *
 * // Later...
 * cleanup();
 * ```
 */
export function createRovingFocus(
  container: HTMLElement,
  options: RovingFocusOptions = {},
): CleanupFunction {
  // SSR guard
  if (typeof window === 'undefined') {
    return () => {};
  }

  const {
    enabled = true,
    orientation = 'horizontal',
    loop = true,
    dir = 'ltr',
    columns,
    currentIndex: initialIndex = 0,
    onNavigate,
    preventDefault = true,
  } = options;

  if (!enabled) {
    return () => {};
  }

  let currentIndex = initialIndex;
  let items = getFocusableItems(container);

  // Initialize tabindex on items
  updateTabindex(items, currentIndex);

  const handleKeyDown = (event: KeyboardEvent) => {
    // Refresh items in case DOM changed
    items = getFocusableItems(container);
    if (items.length === 0) return;

    const { key } = event;

    // Handle Home/End keys
    if (key === 'Home') {
      if (preventDefault) event.preventDefault();
      currentIndex = 0;
      const item = items[currentIndex];
      if (item) {
        updateTabindex(items, currentIndex);
        item.focus();
        onNavigate?.(item, currentIndex);
      }
      return;
    }

    if (key === 'End') {
      if (preventDefault) event.preventDefault();
      currentIndex = items.length - 1;
      const item = items[currentIndex];
      if (item) {
        updateTabindex(items, currentIndex);
        item.focus();
        onNavigate?.(item, currentIndex);
      }
      return;
    }

    // Two-dimensional grid navigation: ArrowLeft/Right move by one column,
    // ArrowUp/Down by a full row, clamped at the edges (no wrap). Reuses the
    // tabstop/focus machinery below; Home/End were already handled above.
    if (columns !== undefined) {
      const delta = gridDelta(key, columns);
      if (delta === null) return;
      if (preventDefault) event.preventDefault();
      currentIndex = Math.max(0, Math.min(currentIndex + delta, items.length - 1));
      const item = items[currentIndex];
      if (item) {
        updateTabindex(items, currentIndex);
        item.focus();
        onNavigate?.(item, currentIndex);
      }
      return;
    }

    // Handle arrow keys
    const { navigate, direction } = shouldNavigate(key, orientation, dir);
    if (!navigate) return;

    if (preventDefault) event.preventDefault();

    currentIndex = getNextIndex(currentIndex, direction, items.length, loop);
    const item = items[currentIndex];
    if (item) {
      updateTabindex(items, currentIndex);
      item.focus();
      onNavigate?.(item, currentIndex);
    }
  };

  // Handle focus within container to track current index
  const handleFocusIn = (event: FocusEvent) => {
    const target = event.target as HTMLElement;
    items = getFocusableItems(container);
    const index = items.indexOf(target);
    if (index !== -1) {
      currentIndex = index;
      updateTabindex(items, currentIndex);
    }
  };

  container.addEventListener('keydown', handleKeyDown);
  container.addEventListener('focusin', handleFocusIn);

  return () => {
    container.removeEventListener('keydown', handleKeyDown);
    container.removeEventListener('focusin', handleFocusIn);
  };
}

/**
 * Focus a specific item by index
 * Utility for programmatic focus management
 */
export function focusItem(container: HTMLElement, index: number): boolean {
  if (typeof window === 'undefined') {
    return false;
  }

  const items = getFocusableItems(container);
  if (index < 0 || index >= items.length) {
    return false;
  }

  updateTabindex(items, index);
  const item = items[index];
  if (item) {
    item.focus();
    return true;
  }
  return false;
}

/**
 * Get current focused item index
 * Returns -1 if no item is focused
 */
export function getCurrentIndex(container: HTMLElement): number {
  if (typeof window === 'undefined') {
    return -1;
  }

  const items = getFocusableItems(container);
  const activeElement = document.activeElement as HTMLElement;
  return items.indexOf(activeElement);
}

/**
 * Refresh tabindex on items (call after DOM changes)
 */
export function refreshRovingFocus(container: HTMLElement, currentIndex = 0): void {
  if (typeof window === 'undefined') {
    return;
  }

  const items = getFocusableItems(container);
  const safeIndex = Math.max(0, Math.min(currentIndex, items.length - 1));
  updateTabindex(items, safeIndex);
}
