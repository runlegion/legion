/**
 * Typeahead primitive
 * Type-to-search functionality for lists and menus
 *
 * Enables users to quickly navigate to items by typing
 * the first few characters of the item text.
 *
 * WCAG Compliance:
 * - 2.1.1 Keyboard (Level A): Keyboard navigation available
 *
 * @example
 * ```typescript
 * const cleanup = createTypeahead(listElement, {
 *   getItems: () => listElement.querySelectorAll('[role="option"]'),
 *   onMatch: (item, index) => {
 *     item.focus();
 *   },
 * });
 * ```
 */

import type { CleanupFunction } from "@/lib/primitives/types";

/**
 * Compute a fuzzy match score between a query and a target string.
 *
 * Scoring heuristics:
 * - +2 for each consecutive matching character
 * - +3 bonus for matching at the start of a word (after space, hyphen, or start of string)
 * - +1 base point per matched character
 *
 * Case insensitive. Empty query matches everything (returns 1).
 * Query longer than target returns 0.
 *
 * @param query - The search query
 * @param target - The string to match against
 * @returns A non-negative score; 0 means no match
 */
export function fuzzyScore(query: string, target: string): number {
  if (query.length === 0) return 1;
  if (query.length > target.length) return 0;

  const q = query.toLowerCase();
  const t = target.toLowerCase();

  let score = 0;
  let qi = 0;
  let prevMatchIndex = -2; // -2 so first match is never "consecutive"

  for (let ti = 0; ti < t.length && qi < q.length; ti++) {
    if (t[ti] === q[qi]) {
      score += 1; // base point
      // consecutive bonus
      if (ti === prevMatchIndex + 1) {
        score += 2;
      }
      // start-of-word bonus
      if (ti === 0 || t[ti - 1] === " " || t[ti - 1] === "-") {
        score += 3;
      }
      prevMatchIndex = ti;
      qi++;
    }
  }

  // All query characters must be found
  return qi === q.length ? score : 0;
}

/**
 * Match mode for typeahead search.
 * - 'prefix': Match from start of text (default, original behavior)
 * - 'fuzzy': Characters must appear in order but not consecutively.
 *   Consecutive matches and start-of-word matches score higher.
 */
export type TypeaheadMatchMode = "prefix" | "fuzzy";

export interface TypeaheadOptions {
  /**
   * Function to get searchable items
   * Called on each keystroke to support dynamic lists
   */
  getItems: () => Iterable<HTMLElement>;

  /**
   * Function to get searchable text from an item
   * @default element.textContent
   */
  getItemText?: (item: HTMLElement) => string;

  /**
   * Callback when a match is found
   */
  onMatch?: (item: HTMLElement, index: number) => void;

  /**
   * Callback when no match is found
   */
  onNoMatch?: (searchString: string) => void;

  /**
   * Time in ms before search string resets
   * @default 1000
   */
  timeout?: number;

  /**
   * Whether to match from start of text only.
   * Only applies when matchMode is 'prefix' (default).
   * Ignored when matchMode is 'fuzzy'.
   * @default true
   */
  matchFromStart?: boolean;

  /**
   * Whether matching is case-sensitive
   * @default false
   */
  caseSensitive?: boolean;

  /**
   * Whether the typeahead is enabled
   * @default true
   */
  enabled?: boolean;

  /**
   * Starting index for circular search
   * If provided, search starts from this index and wraps around
   */
  startIndex?: number;

  /**
   * Match mode for typeahead search.
   * - 'prefix' (default): Matches from start or anywhere in text (controlled by matchFromStart)
   * - 'fuzzy': Characters must appear in order but not consecutively.
   *   Best match (highest score) wins. Used by block-palette for search.
   * @default 'prefix'
   */
  matchMode?: TypeaheadMatchMode;
}

export interface TypeaheadState {
  /**
   * Current search string
   */
  searchString: string;

  /**
   * Index of last matched item
   */
  lastMatchIndex: number;

  /**
   * Whether typeahead is currently active (has typed recently)
   */
  isActive: boolean;
}

/**
 * Default function to extract text from an element
 */
function defaultGetItemText(item: HTMLElement): string {
  return item.getAttribute("aria-label") || item.textContent?.trim() || "";
}

/** Check if an element is disabled via attribute or aria-disabled */
function isItemDisabled(item: HTMLElement): boolean {
  return item.hasAttribute("disabled") || item.getAttribute("aria-disabled") === "true";
}

/**
 * Create typeahead search behavior for a container
 * Returns cleanup function
 *
 * @example
 * ```typescript
 * // Basic usage
 * const cleanup = createTypeahead(menu, {
 *   getItems: () => menu.querySelectorAll('[role="menuitem"]'),
 *   onMatch: (item) => item.focus(),
 * });
 *
 * // Custom text extraction
 * const cleanup = createTypeahead(list, {
 *   getItems: () => list.querySelectorAll('li'),
 *   getItemText: (item) => item.dataset.label || item.textContent || '',
 *   timeout: 500,
 * });
 * ```
 */
export function createTypeahead(
  container: HTMLElement,
  options: TypeaheadOptions,
): CleanupFunction {
  if (typeof window === "undefined" || options.enabled === false) {
    return () => {};
  }

  const controlled = createControlledTypeahead(options);
  container.addEventListener("keydown", controlled.handleKeyDown);

  return () => {
    container.removeEventListener("keydown", controlled.handleKeyDown);
    controlled.reset();
  };
}

/**
 * Create a controlled typeahead
 * Returns state getter and handlers instead of managing internally
 * Useful for framework integration
 *
 * @example
 * ```typescript
 * const typeahead = createControlledTypeahead({
 *   getItems: () => items,
 *   onMatch: (item) => setFocusedItem(item),
 * });
 *
 * // In component
 * <ul onKeyDown={typeahead.handleKeyDown}>
 *   {items.map(item => ...)}
 * </ul>
 * ```
 */
export function createControlledTypeahead(options: Omit<TypeaheadOptions, "enabled">): {
  handleKeyDown: (event: KeyboardEvent) => void;
  reset: () => void;
  getState: () => TypeaheadState;
} {
  const {
    getItems,
    getItemText = defaultGetItemText,
    onMatch,
    onNoMatch,
    timeout = 1000,
    matchFromStart = true,
    caseSensitive = false,
    startIndex,
    matchMode = "prefix",
  } = options;

  let searchString = "";
  let timeoutId: ReturnType<typeof setTimeout> | null = null;
  let lastMatchIndex = -1;
  let isActive = false;

  const reset = () => {
    searchString = "";
    isActive = false;
    if (timeoutId !== null) {
      clearTimeout(timeoutId);
      timeoutId = null;
    }
  };

  const findMatch = (
    items: HTMLElement[],
    search: string,
  ): { item: HTMLElement; index: number } | null => {
    const start = startIndex ?? 0;

    if (matchMode === "fuzzy") {
      let bestScore = 0;
      let bestItem: HTMLElement | null = null;
      let bestIndex = -1;

      for (let i = 0; i < items.length; i++) {
        const index = (start + i) % items.length;
        const item = items[index];
        if (!item || isItemDisabled(item)) continue;
        const score = fuzzyScore(search, getItemText(item));
        if (score > bestScore) {
          bestScore = score;
          bestItem = item;
          bestIndex = index;
        }
      }

      return bestItem ? { item: bestItem, index: bestIndex } : null;
    }

    const normalizedSearch = caseSensitive ? search : search.toLowerCase();

    for (let i = 0; i < items.length; i++) {
      const index = (start + i) % items.length;
      const item = items[index];
      if (!item || isItemDisabled(item)) continue;

      const normalizedText = caseSensitive ? getItemText(item) : getItemText(item).toLowerCase();
      const matches = matchFromStart
        ? normalizedText.startsWith(normalizedSearch)
        : normalizedText.includes(normalizedSearch);

      if (matches) {
        return { item, index };
      }
    }

    return null;
  };

  const handleKeyDown = (event: KeyboardEvent) => {
    if (event.ctrlKey || event.altKey || event.metaKey) {
      return;
    }

    const { key } = event;
    if (key.length !== 1) {
      return;
    }

    if (key === " " && searchString === "") {
      return;
    }

    if (timeoutId !== null) {
      clearTimeout(timeoutId);
    }

    searchString += key;
    isActive = true;

    const items = Array.from(getItems());
    if (items.length === 0) {
      reset();
      return;
    }

    const match = findMatch(items, searchString);

    if (match) {
      lastMatchIndex = match.index;
      onMatch?.(match.item, match.index);
    } else {
      onNoMatch?.(searchString);
    }

    timeoutId = setTimeout(reset, timeout);
  };

  const getState = (): TypeaheadState => ({
    searchString,
    lastMatchIndex,
    isActive,
  });

  return {
    handleKeyDown,
    reset,
    getState,
  };
}

/**
 * Highlight matching text in an element
 * Utility for visual feedback during typeahead
 *
 * Note: Uses innerHTML for highlight rendering. Input is derived from
 * the element's own textContent, not from external/untrusted sources.
 *
 * @example
 * ```typescript
 * // In onMatch callback
 * highlightMatch(item, searchString);
 * ```
 */
export function highlightMatch(
  element: HTMLElement,
  searchString: string,
  options: {
    highlightClass?: string;
    caseSensitive?: boolean;
  } = {},
): CleanupFunction {
  if (typeof window === "undefined") {
    return () => {};
  }

  const { highlightClass = "typeahead-highlight", caseSensitive = false } = options;

  const text = element.textContent || "";
  const normalizedSearch = caseSensitive ? searchString : searchString.toLowerCase();
  const normalizedText = caseSensitive ? text : text.toLowerCase();

  const matchIndex = normalizedText.indexOf(normalizedSearch);
  if (matchIndex === -1) {
    return () => {};
  }

  const originalHTML = element.innerHTML;

  const before = text.slice(0, matchIndex);
  const match = text.slice(matchIndex, matchIndex + searchString.length);
  const after = text.slice(matchIndex + searchString.length);

  element.innerHTML = `${before}<mark class="${highlightClass}">${match}</mark>${after}`;

  return () => {
    element.innerHTML = originalHTML;
  };
}
