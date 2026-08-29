/**
 * Outside click/pointer detection primitive
 * Client-only, returns cleanup function
 */

import type { CleanupFunction, OutsideClickHandler } from "@/lib/primitives/types";

/**
 * Listen for clicks outside an element
 * Returns cleanup function
 */
export function onOutsideClick(
  element: HTMLElement,
  handler: OutsideClickHandler,
): CleanupFunction {
  if (typeof window === "undefined") {
    return () => {};
  }

  const handleClick = (event: MouseEvent | TouchEvent) => {
    const target = event.target as Node;

    // Check if click is outside the element
    if (!element.contains(target)) {
      handler(event);
    }
  };

  // Use capture phase to intercept before bubbling
  document.addEventListener("mousedown", handleClick, true);
  document.addEventListener("touchstart", handleClick, true);

  return () => {
    document.removeEventListener("mousedown", handleClick, true);
    document.removeEventListener("touchstart", handleClick, true);
  };
}

/**
 * Listen for pointer down outside an element
 * More specific than outside click, matches Radix behavior
 * Handles both mouse and touch via pointerdown (modern unified API)
 * Also listens to touchstart as fallback for environments that don't
 * synthesize pointer events from touch events (e.g., Playwright CT)
 */
export function onPointerDownOutside(
  element: HTMLElement,
  handler: (event: PointerEvent | TouchEvent) => void,
): CleanupFunction {
  if (typeof window === "undefined") {
    return () => {};
  }

  let lastTouchTime = 0;

  const handlePointerDown = (event: PointerEvent) => {
    const target = event.target as Node;

    // Skip pointer events that were synthesized from touch events
    // to prevent duplicate handler calls (touchstart + pointerdown for same touch)
    const timeSinceTouch = Date.now() - lastTouchTime;
    if (timeSinceTouch < 50) {
      return;
    }

    if (!element.contains(target)) {
      handler(event);
    }
  };

  const handleTouchStart = (event: TouchEvent) => {
    const target = event.target as Node;

    lastTouchTime = Date.now();

    if (!element.contains(target)) {
      handler(event);
    }
  };

  // Use capture phase to intercept before bubbling
  // pointerdown handles both mouse and touch events in real browsers
  // touchstart is a fallback for test environments (Playwright CT) that don't
  // synthesize pointer events from touch events
  document.addEventListener("pointerdown", handlePointerDown, true);
  document.addEventListener("touchstart", handleTouchStart, true);

  return () => {
    document.removeEventListener("pointerdown", handlePointerDown, true);
    document.removeEventListener("touchstart", handleTouchStart, true);
  };
}
