/**
 * Collision Detector primitive
 * Floating element positioning with viewport collision detection
 *
 * Used for tooltips, popovers, dropdowns, and any floating UI
 * Automatically adjusts position to stay within viewport
 *
 * @example
 * ```typescript
 * const position = computePosition(anchorElement, floatingElement, {
 *   side: 'bottom',
 *   align: 'center',
 *   avoidCollisions: true,
 * });
 *
 * floatingElement.style.left = `${position.x}px`;
 * floatingElement.style.top = `${position.y}px`;
 * ```
 */

import type { Align, Position, Side } from "@/lib/primitives/types";

export interface CollisionOptions {
  /**
   * Preferred side to position the floating element
   * @default 'bottom'
   */
  side?: Side;

  /**
   * Alignment along the side axis
   * @default 'center'
   */
  align?: Align;

  /**
   * Offset from anchor along the side axis (px)
   * @default 0
   */
  sideOffset?: number;

  /**
   * Offset from anchor along the align axis (px)
   * @default 0
   */
  alignOffset?: number;

  /**
   * Element to use as collision boundary
   * @default viewport
   */
  collisionBoundary?: HTMLElement | null;

  /**
   * Padding from collision boundary edges (px)
   * @default 10
   */
  collisionPadding?: number | { top?: number; right?: number; bottom?: number; left?: number };

  /**
   * Whether to flip to opposite side on collision
   * @default true
   */
  avoidCollisions?: boolean;

  /**
   * Sticky behavior when colliding
   * - 'partial': Allow partial visibility
   * - 'always': Keep fully visible
   * @default 'partial'
   */
  sticky?: "partial" | "always";

  /**
   * Arrow element for positioning
   */
  arrowElement?: HTMLElement | null;

  /**
   * Padding for arrow positioning (px)
   * @default 0
   */
  arrowPadding?: number;
}

export interface CollisionResult extends Position {
  /**
   * Arrow position along the alignment axis
   */
  arrowX?: number;
  arrowY?: number;

  /**
   * Whether position was adjusted due to collision
   */
  hasCollision: boolean;

  /**
   * Collision details for each side
   */
  collisions: {
    top: boolean;
    right: boolean;
    bottom: boolean;
    left: boolean;
  };
}

/**
 * Get opposite side
 */
function getOppositeSide(side: Side): Side {
  const opposites: Record<Side, Side> = {
    top: "bottom",
    bottom: "top",
    left: "right",
    right: "left",
  };
  return opposites[side];
}

/**
 * Get collision padding for each side
 */
function getCollisionPadding(
  padding: number | { top?: number; right?: number; bottom?: number; left?: number },
): { top: number; right: number; bottom: number; left: number } {
  if (typeof padding === "number") {
    return { top: padding, right: padding, bottom: padding, left: padding };
  }
  return {
    top: padding.top ?? 0,
    right: padding.right ?? 0,
    bottom: padding.bottom ?? 0,
    left: padding.left ?? 0,
  };
}

/**
 * Get viewport rect
 */
function getViewportRect(): DOMRect {
  return new DOMRect(0, 0, window.innerWidth, window.innerHeight);
}

/**
 * Anchor accepted by {@link computePosition}.
 * - `HTMLElement`: measured via `getBoundingClientRect()`
 * - `DOMRect`: used directly
 * - `{ x, y }` point: treated as a zero-size rect at the given coordinates
 */
export type Anchor = HTMLElement | DOMRect | { x: number; y: number };

/**
 * Resolve any accepted anchor into the DOMRect the positioning math runs on.
 */
function resolveAnchorRect(anchor: Anchor): DOMRect {
  if ("getBoundingClientRect" in anchor) {
    return anchor.getBoundingClientRect();
  }
  if ("width" in anchor) {
    return anchor;
  }
  return new DOMRect(anchor.x, anchor.y, 0, 0);
}

/**
 * Calculate position for given side and alignment
 */
function calculateBasePosition(
  anchorRect: DOMRect,
  floatingRect: DOMRect,
  side: Side,
  align: Align,
  sideOffset: number,
  alignOffset: number,
): { x: number; y: number } {
  let x = 0;
  let y = 0;

  const isVertical = side === "top" || side === "bottom";

  // Calculate side axis position
  switch (side) {
    case "top":
      y = anchorRect.top - floatingRect.height - sideOffset;
      break;
    case "bottom":
      y = anchorRect.bottom + sideOffset;
      break;
    case "left":
      x = anchorRect.left - floatingRect.width - sideOffset;
      break;
    case "right":
      x = anchorRect.right + sideOffset;
      break;
  }

  // Calculate alignment axis position
  if (isVertical) {
    switch (align) {
      case "start":
        x = anchorRect.left + alignOffset;
        break;
      case "center":
        x = anchorRect.left + anchorRect.width / 2 - floatingRect.width / 2 + alignOffset;
        break;
      case "end":
        x = anchorRect.right - floatingRect.width + alignOffset;
        break;
    }
  } else {
    switch (align) {
      case "start":
        y = anchorRect.top + alignOffset;
        break;
      case "center":
        y = anchorRect.top + anchorRect.height / 2 - floatingRect.height / 2 + alignOffset;
        break;
      case "end":
        y = anchorRect.bottom - floatingRect.height + alignOffset;
        break;
    }
  }

  return { x, y };
}

/**
 * Detect collisions with boundary
 */
function detectCollisions(
  x: number,
  y: number,
  floatingRect: DOMRect,
  boundaryRect: DOMRect,
  padding: { top: number; right: number; bottom: number; left: number },
): { top: boolean; right: boolean; bottom: boolean; left: boolean } {
  return {
    top: y < boundaryRect.top + padding.top,
    right: x + floatingRect.width > boundaryRect.right - padding.right,
    bottom: y + floatingRect.height > boundaryRect.bottom - padding.bottom,
    left: x < boundaryRect.left + padding.left,
  };
}

/**
 * Calculate arrow position
 */
function calculateArrowPosition(
  anchorRect: DOMRect,
  floatingRect: DOMRect,
  side: Side,
  x: number,
  y: number,
  arrowPadding: number,
): { arrowX?: number; arrowY?: number } {
  const isVertical = side === "top" || side === "bottom";

  if (isVertical) {
    // Arrow on horizontal axis
    const anchorCenter = anchorRect.left + anchorRect.width / 2;
    let arrowX = anchorCenter - x;

    // Clamp to floating element bounds with padding
    arrowX = Math.max(arrowPadding, Math.min(arrowX, floatingRect.width - arrowPadding));

    return { arrowX };
  } else {
    // Arrow on vertical axis
    const anchorCenter = anchorRect.top + anchorRect.height / 2;
    let arrowY = anchorCenter - y;

    // Clamp to floating element bounds with padding
    arrowY = Math.max(arrowPadding, Math.min(arrowY, floatingRect.height - arrowPadding));

    return { arrowY };
  }
}

/**
 * Compute optimal position for floating element
 *
 * The anchor may be an `HTMLElement` (measured via `getBoundingClientRect()`),
 * a `DOMRect` (used directly), or a `{ x, y }` point (treated as a zero-size
 * rect at that coordinate -- useful for context menus at a cursor position).
 *
 * @example
 * ```typescript
 * const result = computePosition(anchor, floating, {
 *   side: 'bottom',
 *   align: 'start',
 *   sideOffset: 8,
 *   avoidCollisions: true,
 * });
 *
 * floating.style.transform = `translate(${result.x}px, ${result.y}px)`;
 * floating.dataset.side = result.side;
 * floating.dataset.align = result.align;
 * ```
 */
export function computePosition(
  anchor: Anchor,
  floating: HTMLElement,
  options: CollisionOptions = {},
): CollisionResult {
  // SSR guard
  if (typeof window === "undefined") {
    return {
      x: 0,
      y: 0,
      side: options.side ?? "bottom",
      align: options.align ?? "center",
      hasCollision: false,
      collisions: { top: false, right: false, bottom: false, left: false },
    };
  }

  const {
    side: preferredSide = "bottom",
    align: preferredAlign = "center",
    sideOffset = 0,
    alignOffset = 0,
    collisionBoundary = null,
    collisionPadding = 10,
    avoidCollisions = true,
    arrowElement = null,
    arrowPadding = 0,
  } = options;

  // Get rects
  const anchorRect = resolveAnchorRect(anchor);
  const floatingRect = floating.getBoundingClientRect();
  const boundaryRect = collisionBoundary
    ? collisionBoundary.getBoundingClientRect()
    : getViewportRect();

  const padding = getCollisionPadding(collisionPadding);

  // Calculate initial position
  let side = preferredSide;
  const align = preferredAlign;
  let position = calculateBasePosition(
    anchorRect,
    floatingRect,
    side,
    align,
    sideOffset,
    alignOffset,
  );

  // Detect collisions
  let collisions = detectCollisions(position.x, position.y, floatingRect, boundaryRect, padding);

  const hasCollision = collisions.top || collisions.right || collisions.bottom || collisions.left;

  // Adjust for collisions if enabled
  if (avoidCollisions && hasCollision) {
    const isVertical = side === "top" || side === "bottom";

    // Try flipping side
    if (
      (side === "top" && collisions.top) ||
      (side === "bottom" && collisions.bottom) ||
      (side === "left" && collisions.left) ||
      (side === "right" && collisions.right)
    ) {
      const oppositeSide = getOppositeSide(side);
      const oppositePosition = calculateBasePosition(
        anchorRect,
        floatingRect,
        oppositeSide,
        align,
        sideOffset,
        alignOffset,
      );

      const oppositeCollisions = detectCollisions(
        oppositePosition.x,
        oppositePosition.y,
        floatingRect,
        boundaryRect,
        padding,
      );

      // Use opposite side if it has less/no collision
      const currentSideCollision =
        (side === "top" && collisions.top) ||
        (side === "bottom" && collisions.bottom) ||
        (side === "left" && collisions.left) ||
        (side === "right" && collisions.right);

      const oppositeSideCollision =
        (oppositeSide === "top" && oppositeCollisions.top) ||
        (oppositeSide === "bottom" && oppositeCollisions.bottom) ||
        (oppositeSide === "left" && oppositeCollisions.left) ||
        (oppositeSide === "right" && oppositeCollisions.right);

      if (currentSideCollision && !oppositeSideCollision) {
        side = oppositeSide;
        position = oppositePosition;
        collisions = oppositeCollisions;
      }
    }

    // Adjust alignment axis to fit in boundary
    if (isVertical) {
      // Horizontal adjustment
      if (collisions.left) {
        position.x = boundaryRect.left + padding.left;
      } else if (collisions.right) {
        position.x = boundaryRect.right - floatingRect.width - padding.right;
      }
    } else {
      // Vertical adjustment
      if (collisions.top) {
        position.y = boundaryRect.top + padding.top;
      } else if (collisions.bottom) {
        position.y = boundaryRect.bottom - floatingRect.height - padding.bottom;
      }
    }
  }

  // Calculate arrow position
  const arrowPosition: { arrowX?: number; arrowY?: number } = {};

  if (arrowElement) {
    const arrowPos = calculateArrowPosition(
      anchorRect,
      floatingRect,
      side,
      position.x,
      position.y,
      arrowPadding,
    );
    if (arrowPos.arrowX !== undefined) {
      arrowPosition.arrowX = arrowPos.arrowX;
    }
    if (arrowPos.arrowY !== undefined) {
      arrowPosition.arrowY = arrowPos.arrowY;
    }
  }

  return {
    x: position.x,
    y: position.y,
    side,
    align,
    ...arrowPosition,
    hasCollision,
    collisions,
  };
}

/**
 * Apply position to floating element
 * Convenience function that sets styles and data attributes
 *
 * @example
 * ```typescript
 * applyPosition(anchor, floating, { side: 'bottom' });
 * // Sets transform, data-side, data-align on floating element
 * ```
 */
export function applyPosition(
  anchor: HTMLElement,
  floating: HTMLElement,
  options: CollisionOptions = {},
): CollisionResult {
  const result = computePosition(anchor, floating, options);

  // Apply position
  floating.style.position = "absolute";
  floating.style.left = "0";
  floating.style.top = "0";
  floating.style.transform = `translate(${Math.round(result.x)}px, ${Math.round(result.y)}px)`;

  // Set data attributes for styling
  floating.setAttribute("data-side", result.side);
  floating.setAttribute("data-align", result.align);

  // Apply arrow position if provided
  if (options.arrowElement && (result.arrowX !== undefined || result.arrowY !== undefined)) {
    const arrow = options.arrowElement;
    if (result.arrowX !== undefined) {
      arrow.style.left = `${result.arrowX}px`;
    }
    if (result.arrowY !== undefined) {
      arrow.style.top = `${result.arrowY}px`;
    }
  }

  return result;
}

/**
 * Create auto-updating position
 * Returns cleanup function
 *
 * @example
 * ```typescript
 * const cleanup = autoPosition(anchor, floating, {
 *   side: 'bottom',
 *   avoidCollisions: true,
 * });
 *
 * // Later...
 * cleanup();
 * ```
 */
export function autoPosition(
  anchor: HTMLElement,
  floating: HTMLElement,
  options: CollisionOptions = {},
): () => void {
  // SSR guard
  if (typeof window === "undefined") {
    return () => {};
  }

  // Initial position
  applyPosition(anchor, floating, options);

  // Update on scroll and resize
  const update = () => {
    applyPosition(anchor, floating, options);
  };

  // Use ResizeObserver for anchor/floating size changes
  const resizeObserver = new ResizeObserver(update);
  resizeObserver.observe(anchor);
  resizeObserver.observe(floating);

  // Update on scroll (capture phase to catch all scroll containers)
  window.addEventListener("scroll", update, { capture: true, passive: true });
  window.addEventListener("resize", update, { passive: true });

  return () => {
    resizeObserver.disconnect();
    window.removeEventListener("scroll", update, { capture: true });
    window.removeEventListener("resize", update);
  };
}

/**
 * Get the available space on each side of an anchor
 * Useful for deciding which side to prefer
 *
 * @example
 * ```typescript
 * const space = getAvailableSpace(anchor);
 * if (space.bottom > 200) {
 *   // Enough space below
 * }
 * ```
 */
export function getAvailableSpace(
  anchor: HTMLElement,
  boundary?: HTMLElement | null,
): { top: number; right: number; bottom: number; left: number } {
  if (typeof window === "undefined") {
    return { top: 0, right: 0, bottom: 0, left: 0 };
  }

  const anchorRect = anchor.getBoundingClientRect();
  const boundaryRect = boundary ? boundary.getBoundingClientRect() : getViewportRect();

  return {
    top: anchorRect.top - boundaryRect.top,
    right: boundaryRect.right - anchorRect.right,
    bottom: boundaryRect.bottom - anchorRect.bottom,
    left: anchorRect.left - boundaryRect.left,
  };
}
