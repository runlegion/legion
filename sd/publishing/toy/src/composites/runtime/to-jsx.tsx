import { type ComponentType, createElement, type ReactNode } from 'react';
import type { CompositeBlock, CompositeFile } from './manifest';
import { kebabToPascal, walkBlocks } from './walk-blocks';

export interface ToJsxOptions {
  components?: Record<string, ComponentType<Record<string, unknown>>>;
  fallback?: ComponentType<{ type: string }>;
}

export interface CompositeProps extends ToJsxOptions {
  file?: CompositeFile;
  blocks?: CompositeBlock[];
}

const RESERVED_PROPS = new Set(['key', 'ref', 'children']);

function createVisitor(options: ToJsxOptions) {
  const components = options.components ?? {};

  return (block: CompositeBlock, children: ReactNode[]): ReactNode => {
    const Component = components[block.type] ?? components[kebabToPascal(block.type)];

    if (!Component) {
      if (options.fallback)
        return createElement(options.fallback, { key: block.id, type: block.type });
      return null;
    }

    const props: Record<string, unknown> = { key: block.id };
    if (block.meta) {
      for (const [k, v] of Object.entries(block.meta)) {
        if (!RESERVED_PROPS.has(k)) props[k] = v;
      }
    }

    if (block.content !== undefined) {
      if (Array.isArray(block.content)) {
        props.content = block.content;
      } else {
        children = [String(block.content), ...children];
      }
    }

    return createElement(Component, props, children.length > 0 ? children : undefined);
  };
}

export function toJsx(blocks: CompositeBlock[], options: ToJsxOptions = {}): ReactNode {
  if (blocks.length === 0) return null;
  return walkBlocks(blocks, createVisitor(options), (r) =>
    r.length === 1 ? (r[0] ?? null) : createElement('div', null, ...r),
  );
}

export function Composite({ file, blocks, components, fallback }: CompositeProps): ReactNode {
  const source = file?.blocks ?? blocks ?? [];
  const opts: ToJsxOptions = {};
  if (components) opts.components = components;
  if (fallback) opts.fallback = fallback;
  return toJsx(source, opts);
}

export function createComposites(
  composites: Record<string, CompositeFile>,
  options: ToJsxOptions = {},
): Record<string, ComponentType<Partial<ToJsxOptions>>> {
  const result: Record<string, ComponentType<Partial<ToJsxOptions>>> = {};

  for (const [name, file] of Object.entries(composites)) {
    const Component = (props: Partial<ToJsxOptions> = {}): ReactNode => {
      const merged: CompositeProps = { file };
      const c = props.components ?? options.components;
      const f = props.fallback ?? options.fallback;
      if (c) merged.components = c;
      if (f) merged.fallback = f;
      return createElement(Composite, merged);
    };
    Component.displayName = name;
    result[name] = Component;
  }

  return result;
}
