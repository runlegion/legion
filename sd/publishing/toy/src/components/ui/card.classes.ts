import { resolveFillName } from '@/lib/primitives/fill-resolver';
import type { CardConfig, CardState } from '@/components/ui/card.behavior';

export interface CardClassSet {
  root: string;
}

/**
 * Structure the surface always carries: an elevated, bordered, rounded
 * panel. Colour is kept separate so a resolved `fill` can replace the
 * default `bg-card` surface without leaving a competing `bg-*` behind.
 *
 * NO MOTION, AND THE ROW SAYS WHY IT IS MISSING RATHER THAN THIS FILE FAKING
 * ONE. `card / root / hover (when interactive)` in
 * `packages/ui/docs/spec/matrix/motion.jsonl` assigns tier `fast` and curve
 * role `standard` (provenance `baseline`) to an elevation change
 * (`box-shadow`) -- but Card has no interactive variant for that moment to
 * belong to. `CardConfig` is `as` + `fill`, `CardElement` is
 * `div | article | section | aside` (never `a` or `button`), and no hover
 * elevation exists anywhere in the component: the shadow is the constant
 * `shadow-sm` above. A `transition-shadow duration-fast ease-standard` here
 * would time a property that never changes -- the same dead emission the
 * chart-tooltip cells were excluded for. The disagreement between the row and
 * the component is the finding, reported here and not resolved.
 */
const cardStructureClasses =
  'flex flex-col gap-6 rounded-xl border border-card-border py-6 shadow-sm';

/** Default surface token pairing -- card's identity when no fill resolves. */
const cardSurfaceClasses = 'bg-card text-card-foreground';

/**
 * Sub-part classes are config-independent literals, so the framework files
 * import them directly (no context/provider needed for a flat static).
 *
 * Spacing follows shadcn v4: the ROOT owns the vertical rhythm (`flex flex-col
 * gap-6 py-6`) and each part owns only its horizontal inset (`px-6`). That is
 * what lets an arbitrary child dropped straight into a Card -- not wrapped in
 * CardContent -- pick up the same rhythm as the declared parts, which the old
 * per-part `p-6 pt-0` could not do.
 *
 * `text-title-medium ts-title-medium` / `text-body-small ts-body-small` are OURS, not shadcn's raw
 * `font-semibold` / `text-sm`: the semantic typography role tokens are the
 * product, and they are invisible to a swap (same element, same slot, same
 * rendered role).
 */

/**
 * The header is a GRID, per shadcn v4. `auto-rows-min` with
 * `grid-rows-[auto_auto]` stacks title over description; the
 * `has-data-[slot=card-action]:grid-cols-[1fr_auto]` variant opens a second
 * column ONLY when a CardAction is present, which is what makes
 * `cardActionClasses` place instead of no-op.
 */
export const cardHeaderClasses =
  'grid auto-rows-min grid-rows-[auto_auto] items-start gap-1.5 px-6 has-data-[slot=card-action]:grid-cols-[1fr_auto]';

export const cardTitleClasses = 'text-title-medium ts-title-medium leading-none';

export const cardDescriptionClasses = 'text-body-small ts-body-small text-muted-foreground';

export const cardContentClasses = 'px-6';

export const cardFooterClasses = 'flex items-center px-6';

/**
 * Trailing action, positioned into the header grid (shadcn v4 surface). These
 * utilities were inert while the header was `flex flex-col`; the grid header
 * above is the parent they always needed.
 */
export const cardActionClasses = 'col-start-2 row-span-2 row-start-1 self-start justify-self-end';

export function cardClasses(config: CardConfig, _state: CardState): CardClassSet {
  // A resolved fill REPLACES the default surface (bg + paired foreground),
  // so the two never coexist -- no reliance on compiled source-order to pick
  // a winner. An invalid/empty signature keeps the default card surface.
  const fillClasses = config.fill ? resolveFillName(config.fill, 'surface') : '';
  const surface = fillClasses || cardSurfaceClasses;
  return { root: `${surface} ${cardStructureClasses}` };
}
