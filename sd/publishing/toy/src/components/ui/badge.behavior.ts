import type { BehaviorSpec } from '@/lib/contract';

/**
 * Badge: a small label chip, inline. A static score -- no state, no
 * actions, no keymap, no effects. The oracle's variant vocabulary
 * (semantic fills plus the shadcn structural variants) and size scale are
 * config; the score projects nothing because a `<span>` carrying visible
 * text needs no ARIA role of its own -- the text IS the accessible name,
 * read in flow by any assistive technology that reaches it.
 */

/**
 * The vocabularies are the arrays; the unions are DERIVED from them. The
 * DOM-native performances take attributes as `string | null` and must narrow
 * before they can call `badgeClasses`, so the vocabulary needs a runtime form.
 * Deriving the type from the array rather than declaring both keeps them
 * structurally impossible to desync -- the old tree kept a parallel list in its
 * element and quietly lost `link` from it. `badgeClasses` still keys a
 * `Record<BadgeVariant, string>`, so a new entry here fails to compile until it
 * has classes.
 *
 * Narrowing is a parse, not a decision: an unrecognised value narrows to
 * `undefined` and `badgeClasses` applies the single `default` fallback, so no
 * performance ever names the default itself.
 */
export const BADGE_VARIANTS = [
  'default',
  'primary',
  'secondary',
  'destructive',
  'success',
  'warning',
  'info',
  'muted',
  'accent',
  'outline',
  'ghost',
  'link',
] as const;

export const BADGE_SIZES = ['sm', 'default', 'lg'] as const;

export type BadgeVariant = (typeof BADGE_VARIANTS)[number];
export type BadgeSize = (typeof BADGE_SIZES)[number];

export function isBadgeVariant(value: string | null | undefined): value is BadgeVariant {
  return value != null && (BADGE_VARIANTS as ReadonlyArray<string>).includes(value);
}

export function isBadgeSize(value: string | null | undefined): value is BadgeSize {
  return value != null && (BADGE_SIZES as ReadonlyArray<string>).includes(value);
}

export interface BadgeConfig {
  variant?: BadgeVariant | undefined;
  size?: BadgeSize | undefined;
}

export type BadgeState = Record<never, never>;
export type BadgeActions = Record<never, never>;
export type BadgePart = 'root';

export const badge: BehaviorSpec<BadgeConfig, BadgeState, BadgeActions, BadgePart> = {
  name: 'badge',
  parts: { root: {} },
  initialState: () => ({}),
  actions: {},
  canDispatch: () => true,
  // No role: the label text is the entire accessible payload; a decorative
  // inline chip projects nothing for the harness to assert beyond presence.
  aria: () => ({ root: {} }),
  keymap: () => null,
};
