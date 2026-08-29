/**
 * Hover Delay primitive
 * Configurable show/hide delays for tooltips and hover cards
 *
 * Prevents accidental triggers and provides smooth UX for
 * hover-based interactions.
 *
 * @example
 * ```typescript
 * const cleanup = createHoverDelay(triggerElement, {
 *   openDelay: 700,
 *   closeDelay: 300,
 *   onOpen: () => showTooltip(),
 *   onClose: () => hideTooltip(),
 * });
 * ```
 */

import type { CleanupFunction } from "@/lib/primitives/types";

export interface HoverDelayOptions {
  /**
   * Delay in ms before showing content
   * @default 700
   */
  openDelay?: number;

  /**
   * Delay in ms before hiding content
   * @default 300
   */
  closeDelay?: number;

  /**
   * Callback when content should open
   */
  onOpen?: () => void;

  /**
   * Callback when content should close
   */
  onClose?: () => void;

  /**
   * Whether hover behavior is enabled
   * @default true
   */
  enabled?: boolean;

  /**
   * Skip delays (instant show/hide)
   * Useful for touch devices or when user has already
   * seen the tooltip recently
   * @default false
   */
  skipDelays?: boolean;

  /**
   * Also track focus events (for keyboard accessibility)
   * @default true
   */
  trackFocus?: boolean;

  /**
   * Content element(s) to track
   * Hovering over content keeps it open
   */
  contentElement?: HTMLElement | HTMLElement[] | null;
}

export interface HoverDelayState {
  /**
   * Whether content is currently open
   */
  isOpen: boolean;

  /**
   * Whether mouse is over trigger
   */
  isHoveringTrigger: boolean;

  /**
   * Whether mouse is over content
   */
  isHoveringContent: boolean;

  /**
   * Whether trigger is focused
   */
  isFocused: boolean;
}

/**
 * Global hover delay state for coordinating multiple tooltips
 * Allows skipping delays when moving between tooltips
 */
let globalOpenTimestamp = 0;
const SKIP_DELAY_THRESHOLD = 300; // Skip open delay if tooltip was open recently

/**
 * Check if we should skip the open delay
 */
function shouldSkipOpenDelay(): boolean {
  const timeSinceLastOpen = Date.now() - globalOpenTimestamp;
  return timeSinceLastOpen < SKIP_DELAY_THRESHOLD;
}

/**
 * Create hover delay behavior for an element
 * Returns cleanup function
 *
 * @example
 * ```typescript
 * // Basic tooltip
 * const cleanup = createHoverDelay(button, {
 *   openDelay: 700,
 *   closeDelay: 300,
 *   onOpen: () => setTooltipVisible(true),
 *   onClose: () => setTooltipVisible(false),
 * });
 *
 * // With content element (for hovering over tooltip itself)
 * const cleanup = createHoverDelay(trigger, {
 *   contentElement: tooltipElement,
 *   onOpen: () => show(),
 *   onClose: () => hide(),
 * });
 * ```
 */
export function createHoverDelay(
  trigger: HTMLElement,
  options: HoverDelayOptions = {},
): CleanupFunction {
  // SSR guard
  if (typeof window === "undefined") {
    return () => {};
  }

  const {
    openDelay = 700,
    closeDelay = 300,
    onOpen,
    onClose,
    enabled = true,
    skipDelays = false,
    trackFocus = true,
    contentElement = null,
  } = options;

  if (!enabled) {
    return () => {};
  }

  let isOpen = false;
  let isHoveringTrigger = false;
  let isHoveringContent = false;
  let isFocused = false;
  let openTimeoutId: ReturnType<typeof setTimeout> | null = null;
  let closeTimeoutId: ReturnType<typeof setTimeout> | null = null;

  const contentElements = contentElement
    ? Array.isArray(contentElement)
      ? contentElement
      : [contentElement]
    : [];

  /**
   * Clear all pending timeouts
   */
  const clearTimeouts = () => {
    if (openTimeoutId !== null) {
      clearTimeout(openTimeoutId);
      openTimeoutId = null;
    }
    if (closeTimeoutId !== null) {
      clearTimeout(closeTimeoutId);
      closeTimeoutId = null;
    }
  };

  /**
   * Open the content
   */
  const open = () => {
    if (isOpen) return;

    clearTimeouts();
    isOpen = true;
    globalOpenTimestamp = Date.now();
    onOpen?.();
  };

  /**
   * Close the content
   */
  const close = () => {
    if (!isOpen) return;

    clearTimeouts();
    isOpen = false;
    onClose?.();
  };

  /**
   * Schedule opening with delay
   */
  const scheduleOpen = () => {
    if (isOpen) return;

    clearTimeouts();

    const delay = skipDelays || shouldSkipOpenDelay() ? 0 : openDelay;

    if (delay === 0) {
      open();
    } else {
      openTimeoutId = setTimeout(open, delay);
    }
  };

  /**
   * Schedule closing with delay
   */
  const scheduleClose = () => {
    if (!isOpen) return;

    clearTimeouts();

    const delay = skipDelays ? 0 : closeDelay;

    if (delay === 0) {
      close();
    } else {
      closeTimeoutId = setTimeout(close, delay);
    }
  };

  /**
   * Update state and decide whether to open/close
   */
  const updateState = () => {
    const shouldBeOpen = isHoveringTrigger || isHoveringContent || isFocused;

    if (shouldBeOpen) {
      // Cancel any pending close and schedule open if not already open
      if (closeTimeoutId !== null) {
        clearTimeout(closeTimeoutId);
        closeTimeoutId = null;
      }
      if (!isOpen && openTimeoutId === null) {
        scheduleOpen();
      }
    } else {
      // Cancel any pending open and schedule close if open
      if (openTimeoutId !== null) {
        clearTimeout(openTimeoutId);
        openTimeoutId = null;
      }
      if (isOpen && closeTimeoutId === null) {
        scheduleClose();
      }
    }
  };

  // Trigger event handlers
  const handleTriggerMouseEnter = () => {
    isHoveringTrigger = true;
    updateState();
  };

  const handleTriggerMouseLeave = () => {
    isHoveringTrigger = false;
    updateState();
  };

  const handleTriggerFocus = () => {
    if (!trackFocus) return;
    isFocused = true;
    updateState();
  };

  const handleTriggerBlur = () => {
    if (!trackFocus) return;
    isFocused = false;
    updateState();
  };

  // Content event handlers
  const handleContentMouseEnter = () => {
    isHoveringContent = true;
    updateState();
  };

  const handleContentMouseLeave = () => {
    isHoveringContent = false;
    updateState();
  };

  // Add trigger listeners
  trigger.addEventListener("mouseenter", handleTriggerMouseEnter);
  trigger.addEventListener("mouseleave", handleTriggerMouseLeave);

  if (trackFocus) {
    trigger.addEventListener("focus", handleTriggerFocus);
    trigger.addEventListener("blur", handleTriggerBlur);
  }

  // Add content listeners
  for (const element of contentElements) {
    element.addEventListener("mouseenter", handleContentMouseEnter);
    element.addEventListener("mouseleave", handleContentMouseLeave);
  }

  // Cleanup
  return () => {
    clearTimeouts();

    trigger.removeEventListener("mouseenter", handleTriggerMouseEnter);
    trigger.removeEventListener("mouseleave", handleTriggerMouseLeave);

    if (trackFocus) {
      trigger.removeEventListener("focus", handleTriggerFocus);
      trigger.removeEventListener("blur", handleTriggerBlur);
    }

    for (const element of contentElements) {
      element.removeEventListener("mouseenter", handleContentMouseEnter);
      element.removeEventListener("mouseleave", handleContentMouseLeave);
    }
  };
}

/**
 * Create controlled hover delay
 * Returns handlers instead of attaching listeners
 * Useful for framework integration
 *
 * @example
 * ```typescript
 * const hover = createControlledHoverDelay({
 *   onOpen: () => setOpen(true),
 *   onClose: () => setOpen(false),
 * });
 *
 * <button
 *   onMouseEnter={hover.onTriggerEnter}
 *   onMouseLeave={hover.onTriggerLeave}
 *   onFocus={hover.onTriggerFocus}
 *   onBlur={hover.onTriggerBlur}
 * >
 *   Hover me
 * </button>
 * ```
 */
export function createControlledHoverDelay(
  options: Omit<HoverDelayOptions, "enabled" | "contentElement">,
): {
  onTriggerEnter: () => void;
  onTriggerLeave: () => void;
  onTriggerFocus: () => void;
  onTriggerBlur: () => void;
  onContentEnter: () => void;
  onContentLeave: () => void;
  open: () => void;
  close: () => void;
  getState: () => HoverDelayState;
  destroy: () => void;
} {
  const {
    openDelay = 700,
    closeDelay = 300,
    onOpen,
    onClose,
    skipDelays = false,
    trackFocus = true,
  } = options;

  let isOpen = false;
  let isHoveringTrigger = false;
  let isHoveringContent = false;
  let isFocused = false;
  let openTimeoutId: ReturnType<typeof setTimeout> | null = null;
  let closeTimeoutId: ReturnType<typeof setTimeout> | null = null;

  const clearTimeouts = () => {
    if (openTimeoutId !== null) {
      clearTimeout(openTimeoutId);
      openTimeoutId = null;
    }
    if (closeTimeoutId !== null) {
      clearTimeout(closeTimeoutId);
      closeTimeoutId = null;
    }
  };

  const open = () => {
    if (isOpen) return;
    clearTimeouts();
    isOpen = true;
    globalOpenTimestamp = Date.now();
    onOpen?.();
  };

  const close = () => {
    if (!isOpen) return;
    clearTimeouts();
    isOpen = false;
    onClose?.();
  };

  const scheduleOpen = () => {
    if (isOpen) return;
    clearTimeouts();

    const delay = skipDelays || shouldSkipOpenDelay() ? 0 : openDelay;

    if (delay === 0) {
      open();
    } else {
      openTimeoutId = setTimeout(open, delay);
    }
  };

  const scheduleClose = () => {
    if (!isOpen) return;
    clearTimeouts();

    const delay = skipDelays ? 0 : closeDelay;

    if (delay === 0) {
      close();
    } else {
      closeTimeoutId = setTimeout(close, delay);
    }
  };

  const updateState = () => {
    const shouldBeOpen = isHoveringTrigger || isHoveringContent || isFocused;

    if (shouldBeOpen) {
      // Cancel any pending close and schedule open if not already open
      if (closeTimeoutId !== null) {
        clearTimeout(closeTimeoutId);
        closeTimeoutId = null;
      }
      if (!isOpen && openTimeoutId === null) {
        scheduleOpen();
      }
    } else {
      // Cancel any pending open and schedule close if open
      if (openTimeoutId !== null) {
        clearTimeout(openTimeoutId);
        openTimeoutId = null;
      }
      if (isOpen && closeTimeoutId === null) {
        scheduleClose();
      }
    }
  };

  return {
    onTriggerEnter: () => {
      isHoveringTrigger = true;
      updateState();
    },

    onTriggerLeave: () => {
      isHoveringTrigger = false;
      updateState();
    },

    onTriggerFocus: () => {
      if (!trackFocus) return;
      isFocused = true;
      updateState();
    },

    onTriggerBlur: () => {
      if (!trackFocus) return;
      isFocused = false;
      updateState();
    },

    onContentEnter: () => {
      isHoveringContent = true;
      updateState();
    },

    onContentLeave: () => {
      isHoveringContent = false;
      updateState();
    },

    open,
    close,

    getState: (): HoverDelayState => ({
      isOpen,
      isHoveringTrigger,
      isHoveringContent,
      isFocused,
    }),

    destroy: () => {
      clearTimeouts();
    },
  };
}

/**
 * Reset global hover delay state
 * Useful for testing
 */
export function resetHoverDelayState(): void {
  globalOpenTimestamp = 0;
}

/**
 * Check if any tooltip was recently open
 * Useful for external coordination
 */
export function wasRecentlyOpen(): boolean {
  return shouldSkipOpenDelay();
}

/**
 * Create a hover intent detector
 * Only triggers if mouse stays over element for minimum time
 * Helps prevent accidental triggers when mouse passes over element
 *
 * @example
 * ```typescript
 * const cleanup = createHoverIntent(element, {
 *   sensitivity: 7, // px - mouse movement threshold
 *   interval: 100, // ms - polling interval
 *   timeout: 0, // ms - minimum hover time
 *   onEnter: () => showContent(),
 *   onLeave: () => hideContent(),
 * });
 * ```
 */
export function createHoverIntent(
  element: HTMLElement,
  options: {
    sensitivity?: number;
    interval?: number;
    timeout?: number;
    onEnter?: () => void;
    onLeave?: () => void;
  } = {},
): CleanupFunction {
  if (typeof window === "undefined") {
    return () => {};
  }

  const { sensitivity = 7, interval = 100, timeout = 0, onEnter, onLeave } = options;

  let x = 0;
  let y = 0;
  let pX = 0;
  let pY = 0;
  let status = 0; // 0 = not hovering, 1 = hovering
  let timer: ReturnType<typeof setTimeout> | null = null;

  const compare = () => {
    if (timer) clearTimeout(timer);

    if (Math.abs(pX - x) + Math.abs(pY - y) < sensitivity) {
      status = 1;
      onEnter?.();
    } else {
      pX = x;
      pY = y;
      timer = setTimeout(compare, interval);
    }
  };

  const handleMouseMove = (e: MouseEvent) => {
    x = e.clientX;
    y = e.clientY;
  };

  const handleMouseEnter = () => {
    pX = x;
    pY = y;

    if (timer) clearTimeout(timer);

    if (timeout === 0) {
      timer = setTimeout(compare, interval);
    } else {
      timer = setTimeout(() => {
        timer = setTimeout(compare, interval);
      }, timeout);
    }
  };

  const handleMouseLeave = () => {
    if (timer) clearTimeout(timer);

    if (status === 1) {
      status = 0;
      onLeave?.();
    }
  };

  element.addEventListener("mousemove", handleMouseMove);
  element.addEventListener("mouseenter", handleMouseEnter);
  element.addEventListener("mouseleave", handleMouseLeave);

  return () => {
    if (timer) clearTimeout(timer);
    element.removeEventListener("mousemove", handleMouseMove);
    element.removeEventListener("mouseenter", handleMouseEnter);
    element.removeEventListener("mouseleave", handleMouseLeave);
  };
}

/**
 * Options for the composite (menubar-style) hover-intent primitive.
 */
export interface MenuHoverIntentOptions {
  /** Selector matching each trigger instance within the root (its data-value
   *  is the payload passed to onOpen). */
  triggerSelector: string;
  /** Selector matching each content panel within the root; hovering content
   *  keeps the menu open. */
  contentSelector: string;
  /** Delay in ms before a hover opens a trigger (while nothing is open) and
   *  before a leave closes (while something is open). */
  delay: number;
  /**
   * Whether a panel is currently open, read LIVE on each pointer event. While
   * open, entering another trigger switches IMMEDIATELY (menubar style) and
   * leaving schedules a close; while closed, the first open waits out `delay`
   * and a leave never schedules a close. Reading it live keeps every close
   * path (Escape, outside-click, deliberate click) reflected without
   * re-composing the primitive.
   */
  isOpen: () => boolean;
  /** Open (or hover-switch to) the trigger carrying this data-value. */
  onOpen: (value: string) => void;
  /** Close whatever is open. */
  onClose: () => void;
}

/**
 * Composite hover intent over a menubar-style trigger set: the counterpart to
 * the per-pair createHoverDelay/createControlledHoverDelay. N triggers share
 * ONE open slot, opens are value-based (the entered trigger's data-value), and
 * switching is immediate whenever a panel is already open. A single pair of
 * capture-phase pointerenter/pointerleave listeners on the root drives the
 * shared open/close timers.
 *
 * Returns a cleanup that clears pending timers and detaches the listeners.
 */
export function createMenuHoverIntent(
  root: HTMLElement,
  options: MenuHoverIntentOptions,
): CleanupFunction {
  if (typeof window === "undefined") {
    return () => {};
  }

  const { triggerSelector, contentSelector, delay, isOpen, onOpen, onClose } = options;

  let openTimer: ReturnType<typeof setTimeout> | undefined;
  let closeTimer: ReturnType<typeof setTimeout> | undefined;
  const clearOpenTimer = () => {
    if (openTimer !== undefined) {
      clearTimeout(openTimer);
      openTimer = undefined;
    }
  };
  const clearCloseTimer = () => {
    if (closeTimer !== undefined) {
      clearTimeout(closeTimer);
      closeTimer = undefined;
    }
  };

  const onPointerEnter = (event: Event) => {
    const target = event.target as HTMLElement;
    clearCloseTimer();
    if (target.closest(contentSelector)) return;
    const trigger = target.closest<HTMLElement>(`${triggerSelector}:not([disabled])`);
    const value = trigger?.dataset["value"];
    if (!value) return;
    clearOpenTimer();
    if (isOpen()) {
      onOpen(value);
    } else {
      openTimer = setTimeout(() => onOpen(value), delay);
    }
  };

  const onPointerLeave = (event: Event) => {
    const target = event.target as HTMLElement;
    if (!target.closest(`${triggerSelector}, ${contentSelector}`)) return;
    clearOpenTimer();
    if (!isOpen()) return;
    clearCloseTimer();
    closeTimer = setTimeout(() => onClose(), delay);
  };

  root.addEventListener("pointerenter", onPointerEnter, true);
  root.addEventListener("pointerleave", onPointerLeave, true);
  return () => {
    clearOpenTimer();
    clearCloseTimer();
    root.removeEventListener("pointerenter", onPointerEnter, true);
    root.removeEventListener("pointerleave", onPointerLeave, true);
  };
}
