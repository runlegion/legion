/**
 * Pure block-tag resolver
 *
 * Maps a composite block's `type` string to what it renders as. This is the
 * single decision point shared by every render target (currently the Astro
 * engine). Keeping it pure and browser-safe (NO node:fs) means it can be
 * exhaustively unit-tested in isolation and imported from both the package
 * root and the browser-safe `client` entry.
 *
 * Three outcomes:
 *   - `composite:<id>`  -> a nested composite reference, rendered recursively.
 *   - a kebab name       -> a user component (e.g. `button` -> `<Button>`),
 *                           looked up by name in the engine's component map.
 *   - a native tag       -> a plain HTML element. A small alias table maps
 *                           semantic block names (`link`, `image`, `text`,
 *                           `heading`, `list`) to their HTML tags; anything
 *                           else falls back to `div`.
 */

const COMPOSITE_PREFIX = 'composite:';

/**
 * Block types that resolve to a native HTML element rather than a user
 * component. The value is the HTML tag the engine emits.
 */
const NATIVE_TAGS: Record<string, string> = {
  link: 'a',
  image: 'img',
  text: 'span',
  heading: 'h2',
  list: 'ul',
};

/** The block renders a nested composite, looked up by manifest id. */
export interface CompositeResolution {
  kind: 'composite';
  id: string;
}

/** The block renders a user component, looked up by kebab name. */
export interface ComponentResolution {
  kind: 'component';
  name: string;
}

/** The block renders a plain HTML element. */
export interface NativeResolution {
  kind: 'native';
  tag: string;
}

/** The discriminated result of resolving a block `type`. */
export type BlockResolution = CompositeResolution | ComponentResolution | NativeResolution;

/**
 * Resolve a block `type` to its render outcome. Pure -- the same input always
 * yields the same output, with no I/O.
 *
 * - `composite:foo` -> `{ kind: 'composite', id: 'foo' }`.
 * - A known native alias (`link`, `image`, `text`, `heading`, `list`) ->
 *   `{ kind: 'native', tag }`.
 * - Any other kebab name -> `{ kind: 'component', name }`.
 * - An empty/whitespace type -> the default native `div`.
 */
export function resolveBlockTag(type: string): BlockResolution {
  const trimmed = type.trim();

  if (trimmed.startsWith(COMPOSITE_PREFIX)) {
    return { kind: 'composite', id: trimmed.slice(COMPOSITE_PREFIX.length) };
  }

  if (trimmed.length === 0) {
    return { kind: 'native', tag: 'div' };
  }

  const nativeTag = NATIVE_TAGS[trimmed];
  if (nativeTag) {
    return { kind: 'native', tag: nativeTag };
  }

  return { kind: 'component', name: trimmed };
}
