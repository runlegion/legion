/**
 * <rafters-badge> Web Component
 *
 * Framework-target for the Badge component, parallel to badge.tsx (React)
 * and badge.astro (Astro). The inner span carries the SAME utility class
 * strings the React/Astro targets use -- imported from badge.classes.ts --
 * rather than a parallel hand-written CSS map. Presentation resolves from the
 * shared compiled utility sheet adopted by RaftersElement (setUtilityCSS) plus
 * the token custom properties inherited from the host :root.
 *
 * The only shadow-scoped CSS this component owns is the structural :host
 * display shim.
 *
 * Shadow DOM structure: an inner span carrying the composed badge utility
 * classes, wrapping a default slot.
 *
 * Attributes:
 *   variant  default | primary | secondary | destructive | success | warning
 *            | info | muted | accent | outline | ghost
 *   size     sm | default | lg
 *
 * Unknown attribute values fall back to 'default' silently.
 *
 * @cognitive-load 2/10
 * @accessibility Semantic generic span, slotted text remains in the light tree.
 */

import { RaftersElement } from '@/lib/primitives/rafters-element';
import { badgeBaseClasses, badgeSizeClasses, badgeVariantClasses } from '@/components/ui/badge.classes';

export type BadgeVariant =
  | 'default'
  | 'primary'
  | 'secondary'
  | 'destructive'
  | 'success'
  | 'warning'
  | 'info'
  | 'muted'
  | 'accent'
  | 'outline'
  | 'ghost';

export type BadgeSize = 'sm' | 'default' | 'lg';

const BADGE_VARIANTS: ReadonlyArray<BadgeVariant> = [
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
];

const BADGE_SIZES: ReadonlyArray<BadgeSize> = ['sm', 'default', 'lg'];

function parseVariant(value: string | null): BadgeVariant {
  if (value && (BADGE_VARIANTS as ReadonlyArray<string>).includes(value)) {
    return value as BadgeVariant;
  }
  return 'default';
}

function parseSize(value: string | null): BadgeSize {
  if (value && (BADGE_SIZES as ReadonlyArray<string>).includes(value)) {
    return value as BadgeSize;
  }
  return 'default';
}

/**
 * Compose the inner span's class string from the shared class maps.
 * Exported so tests assert the WC renders the exact same composition the
 * Astro target does -- the parity guarantee.
 */
export function composeBadgeClasses(variant: BadgeVariant, size: BadgeSize): string {
  return `${badgeBaseClasses} ${badgeVariantClasses[variant]} ${badgeSizeClasses[size]}`;
}

export class RaftersBadge extends RaftersElement {
  static override styles = ':host { display: inline-flex; }';

  static readonly observedAttributes: ReadonlyArray<string> = ['variant', 'size'];

  override render(): Node {
    const span = document.createElement('span');
    span.className = composeBadgeClasses(
      parseVariant(this.getAttribute('variant')),
      parseSize(this.getAttribute('size')),
    );
    span.appendChild(document.createElement('slot'));
    return span;
  }
}

if (!customElements.get('rafters-badge')) {
  customElements.define('rafters-badge', RaftersBadge);
}
