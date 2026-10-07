/**
 * The binding pass rafters does not ship.
 *
 * The composites README documents `$bind` ("bound values resolve against the
 * consumer's props at render time"), but the installed `toJsx` passes `meta`
 * through untouched: a `{ "$bind": "props.x" }` object reaches the component
 * as a literal object. Nothing in the runtime iterates an array either, and
 * `walkBlocks` renders each child exactly once, bottom-up, so it cannot
 * re-scope a subtree per array item.
 *
 * This pass sits in front of rafters' own renderer: it takes a composite and
 * one document payload and returns a plain `CompositeBlock[]` with every
 * operator resolved and every repeat unrolled. The result goes straight into
 * rafters' `Composite`/`toJsx` unchanged.
 *
 * Operators (each one is something a composite could not say on its own):
 *
 *   value   { "$bind": "props.a.b" }            path into the scope
 *   value   { "$bind": p, "$count": true }      length of an array
 *   value   { "$bind": p, "$map": {..}, "$default": v }  value -> value
 *   value   { "$bind": p, "$join": ", " }       array of strings -> string
 *   value   { "$tpl": "{q.authors} authors" }   string with {path} holes
 *   block   type "repeat", meta { each, as, where? }  one child subtree per item
 *   block   meta "$if" / "$unless"               keep the block on a non-empty value
 *
 * Scope: `props` is the document payload; a repeat adds its `as` name and
 * `$index` (zero-based) and `$n` (one-based).
 */
import type { CompositeBlock, CompositeFile } from "@/composites/runtime/manifest";

type Scope = Readonly<Record<string, unknown>>;

interface WhereClause {
  path: string;
  equals: unknown;
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

export function readPath(scope: Scope, path: string): unknown {
  let current: unknown = scope;
  for (const segment of path.split(".")) {
    if (Array.isArray(current)) {
      const index = Number(segment);
      current = Number.isInteger(index) ? current[index] : undefined;
    } else if (isRecord(current)) {
      current = current[segment];
    } else {
      return undefined;
    }
  }
  return current;
}

function present(value: unknown): boolean {
  if (value === null || value === undefined || value === false) return false;
  if (typeof value === "string") return value.trim().length > 0;
  if (Array.isArray(value)) return value.length > 0;
  return true;
}

function resolveValue(value: unknown, scope: Scope): unknown {
  if (Array.isArray(value)) return value.map((entry) => resolveValue(entry, scope));
  if (!isRecord(value)) return value;

  if (typeof value.$tpl === "string") {
    return value.$tpl.replace(/\{([^}]+)\}/g, (_match, path: string) => {
      const found = readPath(scope, path);
      return found === null || found === undefined ? "" : String(found);
    });
  }

  if (typeof value.$bind === "string") {
    const bound = readPath(scope, value.$bind);
    if (value.$count === true) return Array.isArray(bound) ? bound.length : 0;
    if (typeof value.$join === "string") {
      return Array.isArray(bound) ? bound.map((entry) => String(entry)).join(value.$join) : bound;
    }
    if (isRecord(value.$map)) {
      const key = String(bound);
      return key in value.$map ? value.$map[key] : value.$default;
    }
    return bound;
  }

  const out: Record<string, unknown> = {};
  for (const [key, entry] of Object.entries(value)) out[key] = resolveValue(entry, scope);
  return out;
}

function parseWhere(raw: unknown): WhereClause[] {
  const list = Array.isArray(raw) ? raw : raw === undefined ? [] : [raw];
  const clauses: WhereClause[] = [];
  for (const entry of list) {
    if (isRecord(entry) && typeof entry.path === "string") {
      clauses.push({ path: entry.path, equals: entry.equals });
    }
  }
  return clauses;
}

export function expandComposite(file: CompositeFile, payload: unknown): CompositeBlock[] {
  const byId = new Map<string, CompositeBlock>();
  for (const block of file.blocks) byId.set(block.id, block);
  const out: CompositeBlock[] = [];

  function visit(block: CompositeBlock, scope: Scope, suffix: string, parentId: string | undefined): string[] {
    const meta = block.meta ?? {};
    if ("$if" in meta && !present(resolveValue(meta.$if, scope))) return [];
    if ("$unless" in meta && present(resolveValue(meta.$unless, scope))) return [];

    const childBlocks: CompositeBlock[] = [];
    for (const childId of block.children ?? []) {
      const child = byId.get(childId);
      if (child) childBlocks.push(child);
    }

    if (block.type === "repeat") {
      const items = resolveValue(meta.each, scope);
      if (!Array.isArray(items)) return [];
      const as = typeof meta.as === "string" ? meta.as : "item";
      const where = parseWhere(meta.where);
      const ids: string[] = [];
      let n = 0;
      for (const [index, item] of items.entries()) {
        const itemScope: Scope = { ...scope, [as]: item, $index: index };
        let keep = true;
        for (const clause of where) {
          if (readPath(itemScope, clause.path) !== clause.equals) keep = false;
        }
        if (!keep) continue;
        n += 1;
        const scoped: Scope = { ...itemScope, $n: n };
        for (const child of childBlocks) {
          ids.push(...visit(child, scoped, `${suffix}.${block.id}${index}`, parentId));
        }
      }
      return ids;
    }

    const id = `${block.id}${suffix}`;
    const next: CompositeBlock = { id, type: block.type };
    if (parentId !== undefined) next.parentId = parentId;

    const resolvedMeta: Record<string, unknown> = {};
    for (const [key, entry] of Object.entries(meta)) {
      if (key === "$if" || key === "$unless") continue;
      resolvedMeta[key] = resolveValue(entry, scope);
    }
    if (Object.keys(resolvedMeta).length > 0) next.meta = resolvedMeta;

    if (block.content !== undefined) {
      const content = resolveValue(block.content, scope);
      if (content !== null && content !== undefined) next.content = content;
    }

    out.push(next);
    const childIds: string[] = [];
    for (const child of childBlocks) childIds.push(...visit(child, scope, suffix, id));
    if (childIds.length > 0) next.children = childIds;
    return [id];
  }

  const root: Scope = { props: payload };
  for (const block of file.blocks) {
    if (block.parentId === undefined) visit(block, root, "", undefined);
  }
  return out;
}
