/**
 * Focus trap primitive
 * Client-only, returns cleanup function
 * SSR-safe: checks for window existence
 */

import type { CleanupFunction } from "@/lib/primitives/types";

const FOCUSABLE_SELECTOR = [
  "a[href]",
  "button:not([disabled])",
  "input:not([disabled])",
  "select:not([disabled])",
  "textarea:not([disabled])",
  '[tabindex]:not([tabindex="-1"])',
].join(",");

/**
 * Create a focus trap within an element
 * Returns cleanup function to remove trap
 *
 * @example
 * const cleanup = createFocusTrap(dialogElement);
 * // Later...
 * cleanup();
 */
export function createFocusTrap(element: HTMLElement): CleanupFunction {
  // SSR guard
  if (typeof window === "undefined") {
    return () => {};
  }

  const previouslyFocused = document.activeElement as HTMLElement;

  // Get all focusable elements
  const getFocusableElements = (): HTMLElement[] => {
    return Array.from(element.querySelectorAll(FOCUSABLE_SELECTOR));
  };

  // Focus first element
  const focusableElements = getFocusableElements();
  if (focusableElements.length > 0) {
    focusableElements[0]?.focus();
  }

  // Handle Tab key to cycle focus
  const handleKeyDown = (event: KeyboardEvent) => {
    if (event.key !== "Tab") return;
    const focusable = getFocusableElements();
    if (focusable.length === 0) return;

    const active = document.activeElement as HTMLElement | null;
    const currentIndex = active ? focusable.indexOf(active) : -1;

    if (event.shiftKey) {
      // Shift+Tab: move backward or wrap to last
      const prevIndex = currentIndex > 0 ? currentIndex - 1 : focusable.length - 1;
      const target = focusable[prevIndex];
      if (target) {
        event.preventDefault();
        target.focus();
      }
    } else {
      // Tab: move forward or wrap to first
      const nextIndex =
        currentIndex >= 0 && currentIndex < focusable.length - 1 ? currentIndex + 1 : 0;
      const target = focusable[nextIndex];
      if (target) {
        event.preventDefault();
        target.focus();
      }
    }
  };

  element.addEventListener("keydown", handleKeyDown);

  return () => {
    element.removeEventListener("keydown", handleKeyDown);
    // Restore focus to previously focused element
    previouslyFocused?.focus();
  };
}

/**
 * Prevent scrolling on body when dialog is open
 * Handles iOS Safari elastic scrolling
 * Returns cleanup function
 */
export function preventBodyScroll(): CleanupFunction {
  if (typeof window === "undefined") {
    return () => {};
  }

  const originalOverflow = document.body.style.overflow;
  const originalPaddingRight = document.body.style.paddingRight;
  const originalPosition = document.body.style.position;
  const originalTop = document.body.style.top;
  const originalWidth = document.body.style.width;

  // Store scroll position for iOS Safari fix
  const scrollY = window.scrollY;

  // Prevent scroll but maintain scrollbar width to prevent layout shift
  const scrollbarWidth = window.innerWidth - document.documentElement.clientWidth;
  document.body.style.overflow = "hidden";
  document.body.style.paddingRight = `${scrollbarWidth}px`;

  // iOS Safari fix: position fixed prevents elastic bouncing
  // We need to maintain scroll position visually
  const isIOS =
    /iPad|iPhone|iPod/.test(navigator.userAgent) ||
    (navigator.platform === "MacIntel" && navigator.maxTouchPoints > 1);

  if (isIOS) {
    document.body.style.position = "fixed";
    document.body.style.top = `-${scrollY}px`;
    document.body.style.width = "100%";
  }

  return () => {
    document.body.style.overflow = originalOverflow;
    document.body.style.paddingRight = originalPaddingRight;

    if (isIOS) {
      document.body.style.position = originalPosition;
      document.body.style.top = originalTop;
      document.body.style.width = originalWidth;
      // Restore scroll position
      window.scrollTo(0, scrollY);
    }
  };
}
