/**
 * <rafters-button> -- Web Component button primitive.
 *
 * Mirrors the semantics of button.tsx (variant, size, disabled, type). The
 * inner <button> carries the SAME utility class strings the React and Astro
 * targets use -- imported from button.classes.ts -- rather than a parallel
 * hand-written CSS map. Visual presentation comes from the shared compiled
 * utility stylesheet adopted by RaftersElement (see setUtilityCSS) plus the
 * token custom properties inherited from the host :root.
 *
 * The only shadow-scoped CSS this component owns is the structural :host
 * display shim, which has no React/Astro equivalent because there the
 * <button> IS the outer box; here the host wraps it.
 *
 * Attributes:
 *  - variant: 'default' | 'primary' | 'secondary' | 'destructive' | 'success'
 *             | 'warning' | 'info' | 'muted' | 'accent' | 'outline' | 'ghost'
 *             | 'link'  (default 'default')
 *  - size:    'default' | 'xs' | 'sm' | 'lg' | 'icon' | 'icon-xs'
 *             | 'icon-sm' | 'icon-lg'  (default 'default')
 *  - disabled: boolean (presence-based)
 *  - type:    'button' | 'submit' | 'reset'  (default 'button')
 *
 * Click events bubble naturally from the inner <button> to the host.
 * Default type MUST be 'button' to prevent accidental form submission.
 */

import { RaftersElement } from '@/lib/primitives/rafters-element';
import { buttonBaseClasses, buttonSizeClasses, buttonVariantClasses } from '@/components/ui/button.classes';

export type ButtonVariant =
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
  | 'ghost'
  | 'link';

export type ButtonSize =
  | 'default'
  | 'xs'
  | 'sm'
  | 'lg'
  | 'icon'
  | 'icon-xs'
  | 'icon-sm'
  | 'icon-lg';

export type ButtonType = 'button' | 'submit' | 'reset';

const ALLOWED_VARIANTS: ReadonlyArray<ButtonVariant> = [
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
];

const ALLOWED_SIZES: ReadonlyArray<ButtonSize> = [
  'default',
  'xs',
  'sm',
  'lg',
  'icon',
  'icon-xs',
  'icon-sm',
  'icon-lg',
];

const ALLOWED_TYPES: ReadonlyArray<ButtonType> = ['button', 'submit', 'reset'];

const OBSERVED_ATTRIBUTES: ReadonlyArray<string> = ['variant', 'size', 'disabled', 'type'] as const;

function parseVariant(value: string | null): ButtonVariant {
  if (value && (ALLOWED_VARIANTS as ReadonlyArray<string>).includes(value)) {
    return value as ButtonVariant;
  }
  return 'default';
}

function parseSize(value: string | null): ButtonSize {
  if (value && (ALLOWED_SIZES as ReadonlyArray<string>).includes(value)) {
    return value as ButtonSize;
  }
  return 'default';
}

function parseType(value: string | null): ButtonType {
  if (value && (ALLOWED_TYPES as ReadonlyArray<string>).includes(value)) {
    return value as ButtonType;
  }
  return 'button';
}

/**
 * Compose the inner button's class string from the shared class maps.
 * Exported so tests (and any future tooling) can assert the WC renders the
 * exact same composition the Astro target does -- the parity guarantee.
 */
export function composeButtonClasses(variant: ButtonVariant, size: ButtonSize): string {
  return `${buttonBaseClasses} ${buttonVariantClasses[variant]} ${buttonSizeClasses[size]}`;
}

export class RaftersButton extends RaftersElement {
  static observedAttributes: ReadonlyArray<string> = OBSERVED_ATTRIBUTES;

  /**
   * The only component-owned CSS: the structural host-display shim. Custom
   * elements default to `display: inline`; the inner button needs the host
   * to behave as the outer inline-flex box the React/Astro element is.
   */
  static override styles = ':host { display: inline-flex; }';

  /**
   * Render the inner semantic <button> with a single default <slot>.
   * DOM APIs only -- never innerHTML. The inner button carries the shared
   * utility classes; presentation resolves from the adopted utility sheet.
   */
  override render(): Node {
    const inner = document.createElement('button');
    inner.className = composeButtonClasses(
      parseVariant(this.getAttribute('variant')),
      parseSize(this.getAttribute('size')),
    );
    inner.setAttribute('type', parseType(this.getAttribute('type')));
    inner.disabled = this.hasAttribute('disabled');
    inner.appendChild(document.createElement('slot'));
    return inner;
  }
}

if (typeof customElements !== 'undefined' && !customElements.get('rafters-button')) {
  customElements.define('rafters-button', RaftersButton);
}
