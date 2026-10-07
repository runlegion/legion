/**
 * Composite discovery core
 *
 * Framework-agnostic and browser-safe (NO node:fs). Given raw
 * `.composite.json` strings gathered by an adapter, this validates each
 * against CompositeFileSchema, indexes them by manifest id, detects
 * duplicate ids, and resolves nested `composite:*` references against the
 * gathered set.
 *
 * Adapters (node-fs, vite, ...) are responsible only for FINDING and READING
 * the raw files. They hand the raw entries here. See `discovery-node.ts` and
 * `discovery-vite.ts`.
 */

import type { CompositeFile } from './manifest';
import { CompositeFileSchema } from './manifest';

/** A raw, unparsed `.composite.json` entry handed to the core by an adapter. */
export interface RawCompositeEntry {
  /** The raw file contents (JSON text). */
  raw: string;
  /** Where the entry came from (file path, glob key) -- used in error reports. */
  source: string;
}

/**
 * An adapter's only job: FIND and READ `.composite.json` files and return
 * their raw contents. Adapters may be async (node-fs) or sync (a pre-resolved
 * glob), so the contract allows either.
 */
export type CompositeAdapter = () => Promise<RawCompositeEntry[]> | RawCompositeEntry[];

/** A single discovery failure, tied back to its source for diagnostics. */
export interface DiscoveryError {
  source: string;
  error: string;
}

/** Result of a discovery pass. */
export interface DiscoveryResult {
  /** Valid composites indexed by manifest id (insertion order preserved). */
  registry: Map<string, CompositeFile>;
  /** All failures: invalid JSON, schema errors, duplicate ids. */
  errors: DiscoveryError[];
}

/**
 * Validate, index, and resolve a set of raw composite entries.
 *
 * - Invalid JSON and schema failures are reported in `errors`, never thrown.
 * - The first entry to claim a given manifest id wins; later duplicates are
 *   reported as errors and dropped.
 * - After indexing, every composite's nested `composite:*` references are
 *   checked against the gathered set; unresolved references are reported.
 */
export function discoverComposites(entries: RawCompositeEntry[]): DiscoveryResult {
  const registry = new Map<string, CompositeFile>();
  const errors: DiscoveryError[] = [];

  for (const entry of entries) {
    let parsed: unknown;
    try {
      parsed = JSON.parse(entry.raw);
    } catch (e) {
      errors.push({
        source: entry.source,
        error: `Invalid JSON: ${e instanceof Error ? e.message : String(e)}`,
      });
      continue;
    }

    const result = CompositeFileSchema.safeParse(parsed);
    if (!result.success) {
      errors.push({ source: entry.source, error: result.error.message });
      continue;
    }

    const { id } = result.data.manifest;
    if (registry.has(id)) {
      errors.push({
        source: entry.source,
        error: `Duplicate composite id "${id}" (already registered)`,
      });
      continue;
    }

    registry.set(id, result.data);
  }

  reportUnresolvedReferences(registry, errors);

  return { registry, errors };
}

const COMPOSITE_PREFIX = 'composite:';

/**
 * Walk every composite's blocks and report any nested `composite:*` reference
 * whose target is not in the gathered registry. Cycles and depth are not the
 * concern here -- the render walker (`walk-blocks`) guards those with a visited
 * set and a depth cap; discovery only fails fast on references that can never
 * resolve.
 */
function reportUnresolvedReferences(
  registry: Map<string, CompositeFile>,
  errors: DiscoveryError[],
): void {
  for (const [id, composite] of registry) {
    const missing = new Set<string>();
    for (const block of composite.blocks) {
      if (block.type.startsWith(COMPOSITE_PREFIX)) {
        const refId = block.type.slice(COMPOSITE_PREFIX.length);
        if (!registry.has(refId)) {
          missing.add(refId);
        }
      }
    }

    if (missing.size > 0) {
      errors.push({
        source: id,
        error: `Unresolved composite reference(s): ${[...missing]
          .map((m) => `composite:${m}`)
          .join(', ')}`,
      });
    }
  }
}
