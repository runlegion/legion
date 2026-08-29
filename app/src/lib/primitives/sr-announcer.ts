/**
 * Screen Reader Announcer primitive
 * Live region announcements for screen readers
 *
 * WCAG Compliance:
 * - 4.1.3 Status Messages (Level AA): Status messages communicated to AT
 * - 1.3.1 Info and Relationships (Level A): Relationships conveyed programmatically
 *
 * @see https://www.w3.org/WAI/ARIA/apg/practices/hiding-semantics/#usingaria-liveregions
 *
 * @example
 * ```typescript
 * const announcer = createAnnouncer({ politeness: 'polite' });
 * announcer.announce('3 items loaded');
 *
 * // Later...
 * announcer.destroy();
 * ```
 */

import type { CleanupFunction, LiveRegionPoliteness, LiveRegionRole } from "@/lib/primitives/types";

export interface AnnouncerOptions {
  /**
   * Politeness level for announcements
   * - 'polite': Waits for user to finish current task (default)
   * - 'assertive': Interrupts immediately
   * - 'off': No announcements
   * @default 'polite'
   */
  politeness?: LiveRegionPoliteness;

  /**
   * Whether to clear message after announcing
   * Prevents re-announcement on page refresh
   * @default true
   */
  clearAfterAnnounce?: boolean;

  /**
   * Time in ms before clearing the message
   * @default 1000
   */
  clearTimeout?: number;

  /**
   * ARIA role for the live region
   * - 'status': Non-urgent status message (default for polite)
   * - 'alert': Important time-sensitive message (default for assertive)
   * - 'log': Sequential log of messages
   * @default 'status'
   */
  role?: LiveRegionRole;

  /**
   * Container to append the live region to
   * @default document.body
   */
  container?: HTMLElement;
}

export interface Announcer {
  /**
   * Announce a message to screen readers
   */
  announce: (message: string) => void;

  /**
   * Clear the current announcement
   */
  clear: () => void;

  /**
   * Destroy the announcer and remove the live region
   */
  destroy: CleanupFunction;

  /**
   * Get the live region element
   */
  getElement: () => HTMLElement | null;
}

/**
 * Styles to visually hide the live region while keeping it accessible
 */
const VISUALLY_HIDDEN_STYLES = {
  position: "absolute",
  width: "1px",
  height: "1px",
  padding: "0",
  margin: "-1px",
  overflow: "hidden",
  clip: "rect(0, 0, 0, 0)",
  whiteSpace: "nowrap",
  border: "0",
} as const;

/**
 * Global announcer instances to prevent duplicates
 */
const globalAnnouncers: Map<string, Announcer> = new Map();

/**
 * Create a unique key for announcer deduplication
 */
function getAnnouncerKey(politeness: LiveRegionPoliteness, role: LiveRegionRole): string {
  return `${politeness}-${role}`;
}

/**
 * Create a screen reader announcer
 * Returns object with announce, clear, and destroy methods
 *
 * @example
 * ```typescript
 * // Basic usage
 * const announcer = createAnnouncer();
 * announcer.announce('Form submitted successfully');
 *
 * // Assertive for errors
 * const errorAnnouncer = createAnnouncer({
 *   politeness: 'assertive',
 *   role: 'alert',
 * });
 * errorAnnouncer.announce('Error: Please fix the form');
 *
 * // Cleanup
 * announcer.destroy();
 * ```
 */
export function createAnnouncer(options: AnnouncerOptions = {}): Announcer {
  // SSR guard
  if (typeof window === "undefined") {
    return {
      announce: () => {},
      clear: () => {},
      destroy: () => {},
      getElement: () => null,
    };
  }

  const {
    politeness = "polite",
    clearAfterAnnounce = true,
    clearTimeout: clearDelay = 1000,
    role = politeness === "assertive" ? "alert" : "status",
    container = document.body,
  } = options;

  // Check for existing announcer with same settings
  const key = getAnnouncerKey(politeness, role);
  const existing = globalAnnouncers.get(key);
  if (existing) {
    return existing;
  }

  // Create live region element
  const liveRegion = document.createElement("div");

  // Set ARIA attributes
  liveRegion.setAttribute("aria-live", politeness);
  liveRegion.setAttribute("aria-atomic", "true");
  liveRegion.setAttribute("role", role);

  // Apply visually hidden styles
  Object.assign(liveRegion.style, VISUALLY_HIDDEN_STYLES);

  // Add data attribute for identification
  liveRegion.setAttribute("data-sr-announcer", politeness);

  // Append to container
  container.appendChild(liveRegion);

  let clearTimeoutId: ReturnType<typeof setTimeout> | null = null;

  const announcer: Announcer = {
    announce: (message: string) => {
      // Clear any pending clear timeout
      if (clearTimeoutId !== null) {
        clearTimeout(clearTimeoutId);
        clearTimeoutId = null;
      }

      // Clear first to ensure change is detected
      liveRegion.textContent = "";

      // Use requestAnimationFrame to ensure the clear is processed
      requestAnimationFrame(() => {
        liveRegion.textContent = message;

        // Schedule clear if enabled
        if (clearAfterAnnounce) {
          clearTimeoutId = setTimeout(() => {
            liveRegion.textContent = "";
            clearTimeoutId = null;
          }, clearDelay);
        }
      });
    },

    clear: () => {
      if (clearTimeoutId !== null) {
        clearTimeout(clearTimeoutId);
        clearTimeoutId = null;
      }
      liveRegion.textContent = "";
    },

    destroy: () => {
      if (clearTimeoutId !== null) {
        clearTimeout(clearTimeoutId);
      }
      liveRegion.remove();
      globalAnnouncers.delete(key);
    },

    getElement: () => liveRegion,
  };

  // Store in global map
  globalAnnouncers.set(key, announcer);

  return announcer;
}

/**
 * Convenience function for one-off announcements
 * Creates temporary announcer, announces, then cleans up
 *
 * @example
 * ```typescript
 * // Simple polite announcement
 * announceToScreenReader('Search results updated');
 *
 * // Assertive announcement for errors
 * announceToScreenReader('Form validation failed', 'assertive');
 * ```
 */
export function announceToScreenReader(
  message: string,
  politeness: LiveRegionPoliteness = "polite",
): void {
  if (typeof window === "undefined") return;

  const announcer = createAnnouncer({ politeness });
  announcer.announce(message);
}

/**
 * Create a polite announcer (convenience function)
 * For non-urgent status updates
 *
 * @example
 * ```typescript
 * const announcer = createPoliteAnnouncer();
 * announcer.announce('Loading complete');
 * ```
 */
export function createPoliteAnnouncer(
  options: Omit<AnnouncerOptions, "politeness"> = {},
): Announcer {
  return createAnnouncer({ ...options, politeness: "polite" });
}

/**
 * Create an assertive announcer (convenience function)
 * For urgent messages that should interrupt
 *
 * @example
 * ```typescript
 * const announcer = createAssertiveAnnouncer();
 * announcer.announce('Error: Connection lost');
 * ```
 */
export function createAssertiveAnnouncer(
  options: Omit<AnnouncerOptions, "politeness"> = {},
): Announcer {
  return createAnnouncer({ ...options, politeness: "assertive", role: "alert" });
}

/**
 * Clear all global announcers (for testing)
 */
export function clearAllAnnouncers(): void {
  for (const announcer of globalAnnouncers.values()) {
    announcer.destroy();
  }
  globalAnnouncers.clear();
}

/**
 * Get count of active announcers (for testing/debugging)
 */
export function getAnnouncerCount(): number {
  return globalAnnouncers.size;
}

/**
 * Create an announcer that queues messages
 * Useful when multiple rapid announcements might occur
 *
 * @example
 * ```typescript
 * const announcer = createQueuedAnnouncer({ delay: 500 });
 * announcer.announce('Item 1 added');
 * announcer.announce('Item 2 added'); // Will queue
 * ```
 */
export function createQueuedAnnouncer(
  options: AnnouncerOptions & { delay?: number } = {},
): Announcer & { queue: readonly string[] } {
  if (typeof window === "undefined") {
    return {
      announce: () => {},
      clear: () => {},
      destroy: () => {},
      getElement: () => null,
      queue: [],
    };
  }

  const { delay = 500, ...announcerOptions } = options;
  const baseAnnouncer = createAnnouncer(announcerOptions);

  const messageQueue: string[] = [];
  let isProcessing = false;
  let timeoutId: ReturnType<typeof setTimeout> | null = null;

  const processQueue = () => {
    if (messageQueue.length === 0) {
      isProcessing = false;
      return;
    }

    isProcessing = true;
    const message = messageQueue.shift();
    if (message) {
      baseAnnouncer.announce(message);
    }

    timeoutId = setTimeout(processQueue, delay);
  };

  return {
    announce: (message: string) => {
      messageQueue.push(message);
      if (!isProcessing) {
        processQueue();
      }
    },

    clear: () => {
      messageQueue.length = 0;
      if (timeoutId !== null) {
        clearTimeout(timeoutId);
        timeoutId = null;
      }
      isProcessing = false;
      baseAnnouncer.clear();
    },

    destroy: () => {
      messageQueue.length = 0;
      if (timeoutId !== null) {
        clearTimeout(timeoutId);
      }
      baseAnnouncer.destroy();
    },

    getElement: baseAnnouncer.getElement,

    get queue() {
      return [...messageQueue] as const;
    },
  };
}
