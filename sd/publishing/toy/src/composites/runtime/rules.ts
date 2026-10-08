/**
 * Rule name matching
 *
 * Checks whether composites can connect based on named I/O rules.
 * Matching is by exact name, not structural type comparison.
 */

import type { CompositeFile } from './manifest';

export interface RuleMatch {
  /** Rules that match between output and input */
  matched: string[];
  /** Rules required by input but not produced by output */
  missing: string[];
  /** Rules produced by output but not required by input */
  extra: string[];
  /** Whether all input requirements are satisfied */
  compatible: boolean;
}

/**
 * Check if producer's output satisfies consumer's input.
 */
export function matchRules(producer: CompositeFile, consumer: CompositeFile): RuleMatch {
  const outputSet = new Set(producer.output);
  const inputSet = new Set(consumer.input);

  const matched: string[] = [];
  const missing: string[] = [];

  for (const rule of consumer.input) {
    if (outputSet.has(rule)) {
      matched.push(rule);
    } else {
      missing.push(rule);
    }
  }

  const extra: string[] = [];
  for (const rule of producer.output) {
    if (!inputSet.has(rule)) {
      extra.push(rule);
    }
  }

  return { matched, missing, extra, compatible: missing.length === 0 };
}

/**
 * Derive a composite's boundary I/O from its blocks' open edges.
 *
 * Within a composite, dataflow is by signature: a block output named `X` feeds
 * any block input named `X`. The boundary is what stays unconnected:
 *   - boundary input  = block inputs not produced by any block in the composite
 *   - boundary output = block outputs not consumed by any block in the composite
 *
 * This is how a composite earns its own I/O signature for cross-composite
 * matching (matchRules), instead of declaring it by hand. Order-preserving and
 * de-duplicated. Composition (e.g. `user` = `username` + `password`) is a
 * separate layer that needs declared signature sub-structure -- not handled here.
 */
export function deriveCompositeBoundary(composite: CompositeFile): {
  input: string[];
  output: string[];
} {
  const producedInternally = new Set<string>();
  const consumedInternally = new Set<string>();
  for (const block of composite.blocks) {
    for (const o of block.output ?? []) producedInternally.add(o);
    for (const i of block.input ?? []) consumedInternally.add(i);
  }

  const collect = (
    pick: (block: CompositeFile['blocks'][number]) => string[],
    exclude: Set<string>,
  ) => {
    const out: string[] = [];
    const seen = new Set<string>();
    for (const block of composite.blocks) {
      for (const name of pick(block)) {
        if (!exclude.has(name) && !seen.has(name)) {
          seen.add(name);
          out.push(name);
        }
      }
    }
    return out;
  };

  return {
    input: collect((b) => b.input ?? [], producedInternally),
    output: collect((b) => b.output ?? [], consumedInternally),
  };
}

/**
 * Find all composites that can consume the output of the given producer.
 */
export function findCompatibleConsumers(
  producer: CompositeFile,
  candidates: CompositeFile[],
): CompositeFile[] {
  return candidates.filter((candidate) => matchRules(producer, candidate).compatible);
}

/**
 * Find all composites that can produce the input required by the given consumer.
 */
export function findCompatibleProducers(
  consumer: CompositeFile,
  candidates: CompositeFile[],
): CompositeFile[] {
  return candidates.filter((candidate) => matchRules(candidate, consumer).compatible);
}
