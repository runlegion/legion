/**
 * History primitive for undo/redo functionality
 *
 * Provides state history management with configurable limits,
 * duplicate detection, and batch operations.
 *
 * @example
 * ```typescript
 * const history = createHistory({
 *   initialState: { count: 0 },
 *   limit: 50,
 *   isEqual: (a, b) => a.count === b.count,
 * });
 *
 * history.push({ count: 1 });
 * history.push({ count: 2 });
 *
 * const prev = history.undo(); // { count: 1 }
 * const next = history.redo(); // { count: 2 }
 * ```
 */

import { createMemory, type Memory } from "@/lib/primitives/memory";

export interface HistoryOptions<T> {
  /**
   * Initial state for the history
   */
  initialState: T;

  /**
   * Maximum number of history entries
   * Oldest entries are dropped when exceeded
   * @default 100
   */
  limit?: number;

  /**
   * Equality function to skip duplicate states
   * If returns true, push is skipped
   */
  isEqual?: (a: T, b: T) => boolean;
}

export interface HistoryState<T> {
  /**
   * Current state value
   */
  current: T;

  /**
   * Whether undo is available
   */
  canUndo: boolean;

  /**
   * Whether redo is available
   */
  canRedo: boolean;

  /**
   * Number of states available to undo
   */
  undoCount: number;

  /**
   * Number of states available to redo
   */
  redoCount: number;
}

export interface History<T> {
  /**
   * Get current history state
   */
  getState: () => HistoryState<T>;

  /**
   * Reactive snapshot of the history state. Subscribe or select for undo/redo
   * availability changes instead of polling getState(). Treat as read-only -
   * history owns the writes; consumers observe.
   */
  snapshot: Memory<HistoryState<T>>;

  /**
   * Push a new state to history
   * Clears redo stack and respects limit
   */
  push: (state: T) => void;

  /**
   * Undo to previous state
   * Returns previous state or null if at beginning
   */
  undo: () => T | null;

  /**
   * Redo to next state
   * Returns next state or null if at end
   */
  redo: () => T | null;

  /**
   * Batch multiple push calls into single undo step
   * Only the final state is recorded
   */
  batch: (fn: () => void) => void;

  /**
   * Reset history to initial state only
   */
  clear: () => void;

  /**
   * Whether undo is available
   */
  canUndo: () => boolean;

  /**
   * Whether redo is available
   */
  canRedo: () => boolean;
}

/**
 * Create a history manager for undo/redo functionality
 *
 * @example
 * ```typescript
 * // Basic usage
 * const history = createHistory({ initialState: '' });
 * history.push('hello');
 * history.push('hello world');
 * history.undo(); // 'hello'
 * history.redo(); // 'hello world'
 *
 * // With limit
 * const limited = createHistory({
 *   initialState: 0,
 *   limit: 10,
 * });
 *
 * // With duplicate detection
 * const dedupe = createHistory({
 *   initialState: { x: 0, y: 0 },
 *   isEqual: (a, b) => a.x === b.x && a.y === b.y,
 * });
 *
 * // Batching multiple changes
 * const editor = createHistory({ initialState: '' });
 * editor.batch(() => {
 *   editor.push('a');
 *   editor.push('ab');
 *   editor.push('abc');
 * });
 * // Only 'abc' is recorded, undoing goes back to ''
 * ```
 */
export function createHistory<T>(options: HistoryOptions<T>): History<T> {
  const { initialState, limit = 100, isEqual } = options;

  // Past states (undo stack) - most recent is last
  let past: T[] = [];

  // Current state
  let current: T = initialState;

  // Future states (redo stack) - most recent is first
  let future: T[] = [];

  // Batch mode flag
  let isBatching = false;

  // State before batch started
  let batchStartState: T | null = null;

  // Reactive projection of the history state; recomputed after every mutation.
  const snapshot = createMemory<HistoryState<T>>({
    current,
    canUndo: false,
    canRedo: false,
    undoCount: 0,
    redoCount: 0,
  });

  function sync(): void {
    snapshot.set({
      current,
      canUndo: past.length > 0,
      canRedo: future.length > 0,
      undoCount: past.length,
      redoCount: future.length,
    });
  }

  /**
   * Get current history state
   */
  const getState = (): HistoryState<T> => snapshot.get();

  /**
   * Push a new state to history
   */
  const push = (state: T): void => {
    // Skip if equal to current state
    if (isEqual?.(current, state)) {
      return;
    }

    // In batch mode, just update current without recording or notifying.
    // finalizeBatch is the single sync point for the whole batch.
    if (isBatching) {
      current = state;
      return;
    }

    // Push current to past
    past.push(current);

    // Enforce limit by removing oldest entries
    if (past.length > limit) {
      past = past.slice(past.length - limit);
    }

    // Update current
    current = state;

    // Clear redo stack
    future = [];

    sync();
  };

  /**
   * Undo to previous state
   */
  const undo = (): T | null => {
    if (past.length === 0) {
      return null;
    }

    // Pop from past
    const previous = past.pop();
    if (previous === undefined) {
      return null;
    }

    // Push current to future
    future.unshift(current);

    // Update current
    current = previous;

    sync();
    return current;
  };

  /**
   * Redo to next state
   */
  const redo = (): T | null => {
    if (future.length === 0) {
      return null;
    }

    // Shift from future
    const next = future.shift();
    if (next === undefined) {
      return null;
    }

    // Push current to past
    past.push(current);

    // Update current
    current = next;

    sync();
    return current;
  };

  /**
   * Finalize a batch by recording the state change
   */
  const finalizeBatch = (): void => {
    isBatching = false;

    // If state changed during batch, record it as single undo step
    if (batchStartState !== null) {
      const finalState = current;
      const startState = batchStartState;
      batchStartState = null;

      // Skip if equal to start state
      if (isEqual?.(startState, finalState)) {
        return;
      }

      // Only record if state actually changed
      if (startState !== finalState) {
        // Push start state to past
        past.push(startState);

        // Enforce limit
        if (past.length > limit) {
          past = past.slice(past.length - limit);
        }

        // Clear redo stack
        future = [];

        sync();
      }
    }
  };

  /**
   * Batch multiple push calls into single undo step
   */
  const batch = (fn: () => void): void => {
    if (isBatching) {
      // Nested batch, just run the function
      fn();
      return;
    }

    // Save state before batch
    batchStartState = current;
    isBatching = true;

    try {
      fn();
    } finally {
      finalizeBatch();
    }
  };

  /**
   * Reset history to initial state only
   */
  const clear = (): void => {
    past = [];
    current = initialState;
    future = [];
    isBatching = false;
    batchStartState = null;
    sync();
  };

  /**
   * Whether undo is available
   */
  const canUndo = (): boolean => past.length > 0;

  /**
   * Whether redo is available
   */
  const canRedo = (): boolean => future.length > 0;

  return {
    getState,
    snapshot,
    push,
    undo,
    redo,
    batch,
    clear,
    canUndo,
    canRedo,
  };
}
