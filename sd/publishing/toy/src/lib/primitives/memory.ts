/**
 * memory.ts - a light reactive state cell over nanostores `atom`.
 *
 * Every rafters primitive needs the same thing: one reactive object it can read,
 * replace, partially update, and subscribe to. Before this, each primitive hand-rolled
 * the partial merge `$state.set({ ...$state.get(), x })`. `createMemory` is that pattern
 * extracted once: tiny (a thin wrapper), powerful through composition (`patch` +
 * equality-gated `select` + `derive`/computed), and framework-free so the same cell
 * runs in React, a Web Component, or an Astro `<script>`.
 *
 * Actions live in the primitive that owns the memory; memory is only the reactive
 * container. The React bridge is `useMemory` in `../hooks/use-memory`.
 *
 * @example
 * ```ts
 * const state = createMemory(() => ({ open: false, focusedId: undefined as string | undefined }));
 * state.patch({ open: true });           // shallow merge
 * const stop = state.select(             // fine-grained: fires only when `open` changes
 *   (s) => s.open,
 *   (isOpen) => console.info(isOpen),
 * );
 * state.reset();                          // back to a fresh initial value
 * stop();
 * ```
 */
import { atom, computed, type ReadableAtom } from 'nanostores';

export interface Memory<T> {
  /** Current value. */
  get(): T;
  /** Replace the whole value. */
  set(value: T): void;
  /** Shallow partial merge - replaces the `{ ...get(), x }` spread. */
  patch(partial: Partial<T>): void;
  /** Re-seat the initial value (a fresh value when constructed with a factory). */
  reset(): void;
  /** Subscribe to the whole value. Fires immediately with the current value (nanostores semantics). */
  subscribe(listener: (value: T) => void): () => void;
  /**
   * Subscribe to one slice, equality-gated. Fires only when the selected slice changes
   * (default `Object.is`); does not fire on initial subscribe.
   */
  select<S>(
    selector: (value: T) => S,
    listener: (slice: S) => void,
    isEqual?: (a: S, b: S) => boolean,
  ): () => void;
  /** A derived (computed) read-only store over this memory. */
  derive<S>(selector: (value: T) => S): ReadableAtom<S>;
  /** The underlying readable atom - escape hatch for nanostores-native interop. */
  readonly atom: ReadableAtom<T>;
}

/**
 * Create a reactive memory cell.
 *
 * Pass a factory (`() => T`) when `T` holds reference types (Set, array, object) so that
 * `reset()` yields a fresh value instead of re-seating the same shared instance.
 */
export function createMemory<T>(initial: T | (() => T)): Memory<T> {
  const make = (typeof initial === 'function' ? initial : () => initial) as () => T;
  const store = atom<T>(make());

  return {
    atom: store,
    get: () => store.get(),
    set: (value) => {
      store.set(value);
    },
    patch: (partial) => {
      store.set({ ...store.get(), ...partial });
    },
    reset: () => {
      store.set(make());
    },
    subscribe: (listener) => store.subscribe(listener),
    select<S>(
      selector: (value: T) => S,
      listener: (slice: S) => void,
      isEqual: (a: S, b: S) => boolean = Object.is,
    ): () => void {
      let previous = selector(store.get());
      return store.subscribe((value) => {
        const next = selector(value);
        if (!isEqual(previous, next)) {
          previous = next;
          listener(next);
        }
      });
    },
    derive<S>(selector: (value: T) => S): ReadableAtom<S> {
      return computed(store, selector);
    },
  };
}
