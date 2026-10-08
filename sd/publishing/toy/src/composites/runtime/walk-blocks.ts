import type { CompositeBlock } from './manifest';

const MAX_DEPTH = 50;

export type BlockVisitor<T> = (block: CompositeBlock, children: T[]) => T;

function walk<T>(
  block: CompositeBlock,
  blockMap: Map<string, CompositeBlock>,
  visitor: BlockVisitor<T>,
  visited: Set<string>,
  depth: number,
): T | null {
  if (depth > MAX_DEPTH || visited.has(block.id)) return null;
  visited.add(block.id);

  const childResults: T[] = [];
  if (block.children) {
    for (const childId of block.children) {
      const child = blockMap.get(childId);
      if (!child) continue;
      const result = walk(child, blockMap, visitor, visited, depth + 1);
      if (result !== null) childResults.push(result);
    }
  }

  return visitor(block, childResults);
}

export function walkBlocks<T>(
  blocks: CompositeBlock[],
  visitor: BlockVisitor<T>,
  join: (results: T[]) => T,
): T {
  const blockMap = new Map<string, CompositeBlock>();
  for (const block of blocks) blockMap.set(block.id, block);

  const results: T[] = [];
  for (const block of blocks) {
    if (block.parentId) continue;
    const result = walk(block, blockMap, visitor, new Set(), 0);
    if (result !== null) results.push(result);
  }

  return join(results);
}

export function kebabToPascal(kebab: string): string {
  const sanitized = kebab.replace(/[^a-zA-Z0-9-]/g, '');
  const parts = sanitized.split('-').filter((p) => p.length > 0);
  if (parts.length === 0) return '';
  return parts.map((part) => part.charAt(0).toUpperCase() + part.slice(1)).join('');
}
