import type { BehaviorSpec } from '@/lib/contract';

/**
 * Typography: a STATIC score, in the grain of container. The semantic text
 * set (H1-H6, P, Code, Small, Blockquote, List, and the presentational
 * variants) has no state, no actions, no keymap, and no effects -- its whole
 * contract is structural: a variant chooses a native text element (the score's
 * `variantToTag`) and the classes file dresses it with token-prop utilities.
 * The element IS the accessibility contract (a heading is a heading, a
 * blockquote is a quotation), which is why the aria projection is empty and
 * the harness asserts the ELEMENT, not a projected role.
 *
 * The score owns two pure structural decisions so they survive with no DOM:
 * - `variantToTag` / `resolveVariant`: variant -> semantic tag, unknown -> `p`.
 * - `variantForElement`: the generic `as`-element -> variant defaulting the
 *   Astro/React wrappers use (h5/h6 borrow h4's scale; span reads as body).
 */

export type TypographyVariant =
  | 'h1'
  | 'h2'
  | 'h3'
  | 'h4'
  | 'p'
  | 'lead'
  | 'large'
  | 'small'
  | 'muted'
  | 'code'
  | 'codeblock'
  | 'blockquote'
  | 'mark'
  | 'abbr'
  | 'ul'
  | 'ol'
  | 'li';

/**
 * The native element a consumer may hand the generic wrapper via `as`. A
 * superset of the tags variants render (adds h5/h6/span), because the generic
 * component lets the element drive and derives the variant from it.
 */
export type TypographyElement =
  | 'h1'
  | 'h2'
  | 'h3'
  | 'h4'
  | 'h5'
  | 'h6'
  | 'p'
  | 'blockquote'
  | 'code'
  | 'small'
  | 'span'
  | 'mark';

/**
 * Per-dimension token-prop overrides. Each replaces the matching dimension of
 * the variant's defaults at emit time (see typography.classes.ts). `undefined`
 * is explicit for exactOptionalPropertyTypes.
 */
export interface TypographyTokenProps {
  size?: string | undefined;
  weight?: string | undefined;
  color?: string | undefined;
  line?: string | undefined;
  tracking?: string | undefined;
  family?: string | undefined;
  align?: string | undefined;
  transform?: string | undefined;
}

export interface TypographyConfig extends TypographyTokenProps {
  variant?: TypographyVariant | undefined;
}

export type TypographyState = Record<never, never>;
export type TypographyActions = Record<never, never>;
export type TypographyPart = 'root';

/**
 * Variant -> semantic HTML tag (the element the wrappers render). Unknown
 * variants fall back to `p` via `resolveVariant` -- this map NEVER throws.
 * `codeblock` renders `pre` (its wrapper nests a `code` inside).
 */
export const variantToTag: Record<TypographyVariant, string> = {
  h1: 'h1',
  h2: 'h2',
  h3: 'h3',
  h4: 'h4',
  p: 'p',
  lead: 'p',
  large: 'p',
  small: 'small',
  muted: 'p',
  code: 'code',
  codeblock: 'pre',
  blockquote: 'blockquote',
  mark: 'mark',
  abbr: 'abbr',
  ul: 'ul',
  ol: 'ol',
  li: 'li',
};

/**
 * Coerce an arbitrary value to a known variant. Unknown or empty values fall
 * back to `p`. NEVER throws -- the WC reads an untrusted attribute through this.
 */
export function resolveVariant(value: unknown): TypographyVariant {
  if (typeof value !== 'string' || value.length === 0) return 'p';
  if (value in variantToTag) return value as TypographyVariant;
  return 'p';
}

/**
 * The generic wrapper's element -> variant defaulting. `span` reads as body
 * text (`p`); h5/h6 have no variant scale of their own and borrow h4's; any
 * other element that is itself a variant maps to that variant. Faithful to the
 * oracle's Astro resolution -- the variant vocabulary stops at h4, the tags do
 * not.
 */
export function variantForElement(element: TypographyElement): TypographyVariant {
  if (element === 'span') return 'p';
  if (element === 'h5' || element === 'h6') return 'h4';
  return resolveVariant(element);
}

/**
 * The static score. Empty everywhere an interactive score is not: no state to
 * hold, no actions to dispatch, no keys to bind, and (Spec 03 is gone) no
 * effects to run. The one part is the rendered text element; its ARIA is the
 * empty projection because the semantic tag carries the meaning natively.
 */
export const typography: BehaviorSpec<
  TypographyConfig,
  TypographyState,
  TypographyActions,
  TypographyPart
> = {
  name: 'typography',
  parts: { root: {} },
  initialState: () => ({}),
  actions: {},
  canDispatch: () => true,
  aria: () => ({ root: {} }),
  keymap: () => null,
};
