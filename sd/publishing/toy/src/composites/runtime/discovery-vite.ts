/**
 * Vite composite adapter (browser-safe)
 *
 * Wraps the output of `import.meta.glob('/**\/*.composite.json', { eager: true,
 * query: '?raw', import: 'default' })` into raw discovery entries.
 *
 * The glob pattern MUST be a string literal at the consumer's call site --
 * Vite statically analyzes it and cannot resolve a pattern passed through a
 * function. So the consumer calls `import.meta.glob(...)` itself and hands the
 * resulting record to this adapter, which normalizes it.
 *
 * No node:fs here -- safe to export from `client.ts`.
 */

import type { CompositeAdapter, DiscoveryResult, RawCompositeEntry } from './discovery';
import { discoverComposites } from './discovery';

/**
 * The shape of an eager `?raw` glob result: a map of module path -> raw file
 * contents. Matches `import.meta.glob(pattern, { eager: true, query: '?raw',
 * import: 'default' })`.
 */
export type ViteRawGlob = Record<string, string>;

/**
 * Normalize a Vite eager-raw glob record into raw discovery entries. The glob
 * keys become the entry `source` values.
 */
export function viteGlobEntries(glob: ViteRawGlob): RawCompositeEntry[] {
  return Object.entries(glob).map(([source, raw]) => ({ source, raw }));
}

/**
 * Build a {@link CompositeAdapter} from a Vite eager-raw glob record.
 *
 * @example
 * const adapter = viteAdapter(
 *   import.meta.glob('/src/composites/**\/*.composite.json', {
 *     eager: true,
 *     query: '?raw',
 *     import: 'default',
 *   }) as ViteRawGlob,
 * );
 */
export function viteAdapter(glob: ViteRawGlob): CompositeAdapter {
  return () => viteGlobEntries(glob);
}

/**
 * Convenience: discover composites directly from a Vite eager-raw glob record.
 */
export function discoverFromVite(glob: ViteRawGlob): DiscoveryResult {
  return discoverComposites(viteGlobEntries(glob));
}
