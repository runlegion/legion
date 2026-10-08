/**
 * React bridge for a Memory cell.
 *
 * Subscribes a component to the whole memory value via `useSyncExternalStore`, which is
 * concurrent-safe and SSR-safe (the server snapshot is the deterministic current value).
 * The memory core stays framework-free; this hook is the only React-aware piece.
 *
 * @example
 * ```tsx
 * const state = createMemory(() => ({ open: false }));
 * function Panel() {
 *   const { open } = useMemory(state);
 *   return open ? <div /> : null;
 * }
 * ```
 */
import { useSyncExternalStore } from 'react';
import type { Memory } from '@/lib/primitives/memory';

/** Subscribe a React component to the whole value of a Memory cell. */
export function useMemory<T>(memory: Memory<T>): T {
  return useSyncExternalStore(memory.subscribe, memory.get, memory.get);
}
