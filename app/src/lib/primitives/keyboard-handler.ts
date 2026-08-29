/**
 * Keyboard Handler primitive
 * Type-safe keyboard event handling with modifier support
 *
 * WCAG Compliance:
 * - 2.1.1 Keyboard (Level A): Correct keyboard event handling
 * - 2.1.4 Character Key Shortcuts (Level A): Modifier key support prevents AT conflicts
 *
 * @example
 * ```typescript
 * const cleanup = createKeyboardHandler(element, {
 *   key: ['Enter', 'Space'],
 *   handler: (event) => activateButton(),
 *   preventDefault: true,
 * });
 * ```
 */

import type {
  CleanupFunction,
  KeyboardHandlerCallback,
  KeyboardKey,
  KeyboardModifiers,
} from "@/lib/primitives/types";

export interface KeyboardHandlerOptions {
  /**
   * Whether the handler is enabled
   * @default true
   */
  enabled?: boolean;

  /**
   * Key(s) to handle
   */
  key: KeyboardKey | KeyboardKey[];

  /**
   * Handler function called when key is pressed
   */
  handler: KeyboardHandlerCallback;

  /**
   * Required modifier keys
   * If specified, handler only fires when modifiers match
   */
  modifiers?: KeyboardModifiers;

  /**
   * Prevent default browser behavior for handled keys
   * @default false
   */
  preventDefault?: boolean;

  /**
   * Stop event propagation for handled keys
   * @default false
   */
  stopPropagation?: boolean;

  /**
   * Use capture phase for event listener
   * @default false
   */
  capture?: boolean;
}

/**
 * Map from our KeyboardKey type to actual event.key values
 */
const KEY_MAP: Record<KeyboardKey, string> = {
  Enter: "Enter",
  Space: " ",
  Escape: "Escape",
  Tab: "Tab",
  ArrowUp: "ArrowUp",
  ArrowDown: "ArrowDown",
  ArrowLeft: "ArrowLeft",
  ArrowRight: "ArrowRight",
  Home: "Home",
  End: "End",
  PageUp: "PageUp",
  PageDown: "PageDown",
  Backspace: "Backspace",
  Delete: "Delete",
};

/**
 * Check if event modifiers match required modifiers
 */
function matchesModifiers(event: KeyboardEvent, modifiers?: KeyboardModifiers): boolean {
  if (!modifiers) return true;

  const { shift, ctrl, alt, meta } = modifiers;

  // Check each modifier if specified
  if (shift !== undefined && event.shiftKey !== shift) return false;
  if (ctrl !== undefined && event.ctrlKey !== ctrl) return false;
  if (alt !== undefined && event.altKey !== alt) return false;
  if (meta !== undefined && event.metaKey !== meta) return false;

  return true;
}

/**
 * Check if event key matches one of the target keys
 */
function matchesKey(event: KeyboardEvent, keys: KeyboardKey[]): boolean {
  const eventKey = event.key;

  for (const key of keys) {
    const mappedKey = KEY_MAP[key];
    if (eventKey === mappedKey) return true;
  }

  return false;
}

/**
 * Create a keyboard event handler for specific keys
 * Returns cleanup function to remove listener
 *
 * @example
 * ```typescript
 * // Handle Enter and Space for button activation
 * const cleanup = createKeyboardHandler(button, {
 *   key: ['Enter', 'Space'],
 *   handler: () => button.click(),
 *   preventDefault: true,
 * });
 *
 * // Handle Escape with Shift modifier
 * const cleanup2 = createKeyboardHandler(dialog, {
 *   key: 'Escape',
 *   modifiers: { shift: true },
 *   handler: () => closeAll(),
 * });
 * ```
 */
export function createKeyboardHandler(
  element: HTMLElement | Document,
  options: KeyboardHandlerOptions,
): CleanupFunction {
  // SSR guard
  if (typeof window === "undefined") {
    return () => {};
  }

  const {
    enabled = true,
    key,
    handler,
    modifiers,
    preventDefault = false,
    stopPropagation = false,
    capture = false,
  } = options;

  if (!enabled) {
    return () => {};
  }

  // Normalize key to array
  const keys: KeyboardKey[] = Array.isArray(key) ? key : [key];

  const handleKeyDown = (event: Event) => {
    const keyboardEvent = event as KeyboardEvent;

    // Check if key matches
    if (!matchesKey(keyboardEvent, keys)) return;

    // Check if modifiers match
    if (!matchesModifiers(keyboardEvent, modifiers)) return;

    // Handle the event
    if (preventDefault) keyboardEvent.preventDefault();
    if (stopPropagation) keyboardEvent.stopPropagation();

    handler(keyboardEvent);
  };

  element.addEventListener("keydown", handleKeyDown, { capture });

  return () => {
    element.removeEventListener("keydown", handleKeyDown, { capture });
  };
}

/**
 * Create handler for button activation (Enter and Space)
 * Convenience function for common pattern
 *
 * @example
 * ```typescript
 * const cleanup = createActivationHandler(customButton, () => {
 *   performAction();
 * });
 * ```
 */
export function createActivationHandler(
  element: HTMLElement,
  onActivate: KeyboardHandlerCallback,
): CleanupFunction {
  return createKeyboardHandler(element, {
    key: ["Enter", "Space"],
    handler: onActivate,
    preventDefault: true,
  });
}

/**
 * Create handler for dismissal (Escape key)
 * Convenience function for common pattern
 *
 * @example
 * ```typescript
 * const cleanup = createDismissalHandler(dialog, () => {
 *   closeDialog();
 * });
 * ```
 */
export function createDismissalHandler(
  element: HTMLElement | Document,
  onDismiss: KeyboardHandlerCallback,
): CleanupFunction {
  return createKeyboardHandler(element, {
    key: "Escape",
    handler: onDismiss,
    // Don't prevent default for Escape - let it bubble for nested dismissables
  });
}

/**
 * Create handler for arrow key navigation
 * Convenience function for directional navigation
 *
 * @example
 * ```typescript
 * const cleanup = createNavigationHandler(list, {
 *   onNext: () => focusNextItem(),
 *   onPrevious: () => focusPreviousItem(),
 *   orientation: 'vertical',
 * });
 * ```
 */
export function createNavigationHandler(
  element: HTMLElement,
  callbacks: {
    onNext?: KeyboardHandlerCallback;
    onPrevious?: KeyboardHandlerCallback;
    onFirst?: KeyboardHandlerCallback;
    onLast?: KeyboardHandlerCallback;
    orientation?: "horizontal" | "vertical" | "both";
  },
): CleanupFunction {
  const { onNext, onPrevious, onFirst, onLast, orientation = "vertical" } = callbacks;

  const cleanups: CleanupFunction[] = [];

  // Determine which keys trigger next/previous based on orientation
  const nextKeys: KeyboardKey[] = [];
  const prevKeys: KeyboardKey[] = [];

  if (orientation === "vertical" || orientation === "both") {
    nextKeys.push("ArrowDown");
    prevKeys.push("ArrowUp");
  }

  if (orientation === "horizontal" || orientation === "both") {
    nextKeys.push("ArrowRight");
    prevKeys.push("ArrowLeft");
  }

  if (onNext && nextKeys.length > 0) {
    cleanups.push(
      createKeyboardHandler(element, {
        key: nextKeys,
        handler: onNext,
        preventDefault: true,
      }),
    );
  }

  if (onPrevious && prevKeys.length > 0) {
    cleanups.push(
      createKeyboardHandler(element, {
        key: prevKeys,
        handler: onPrevious,
        preventDefault: true,
      }),
    );
  }

  if (onFirst) {
    cleanups.push(
      createKeyboardHandler(element, {
        key: "Home",
        handler: onFirst,
        preventDefault: true,
      }),
    );
  }

  if (onLast) {
    cleanups.push(
      createKeyboardHandler(element, {
        key: "End",
        handler: onLast,
        preventDefault: true,
      }),
    );
  }

  return () => {
    for (const cleanup of cleanups) {
      cleanup();
    }
  };
}

/**
 * Create a combined keyboard handler for multiple key bindings
 * Useful for complex keyboard interaction patterns
 *
 * @example
 * ```typescript
 * const cleanup = createKeyBindings(element, [
 *   { key: 'Enter', handler: activateItem },
 *   { key: 'Delete', handler: deleteItem },
 *   { key: 'Escape', handler: deselectAll },
 * ]);
 * ```
 */
export function createKeyBindings(
  element: HTMLElement | Document,
  bindings: KeyboardHandlerOptions[],
): CleanupFunction {
  const cleanups = bindings.map((binding) => createKeyboardHandler(element, binding));

  return () => {
    for (const cleanup of cleanups) {
      cleanup();
    }
  };
}
