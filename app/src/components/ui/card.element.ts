/**
 * <rafters-card> Web Component
 *
 * Third framework target alongside card.tsx (React) and card.astro (Astro).
 * Each inner node carries the SAME utility class strings the React/Astro
 * targets use -- imported from card.classes.ts -- rather than a parallel
 * hand-written CSS map. Presentation resolves from the shared compiled utility
 * sheet adopted by RaftersElement (setUtilityCSS) plus the token custom
 * properties inherited from the host :root.
 *
 * The only shadow-scoped CSS this component owns is the structural :host
 * display shim. Hover, focus-visible, and reduced-motion behaviour all live in
 * the cardInteractiveClasses utility string, so no per-instance stylesheet is
 * needed any more.
 *
 * Structure: single .card wrapper containing named slots for header, action,
 * content, footer, plus a default slot for unnamed children.
 *
 * Attributes:
 * - interactive: boolean (presence-based). When present, applies the hover and
 *   focus-visible utility classes, sets tabindex="0" and role="button" on the
 *   host (unless the consumer already provided them), and dispatches a
 *   bubbling, composed 'rafters-card-activate' CustomEvent on Enter or Space.
 * - background: CardBackground (none | muted | accent | card | primary |
 *   secondary). Unknown values fall back to 'card' silently.
 */

import { RaftersElement } from '@/lib/primitives/rafters-element';
import {
  type CardBackground,
  cardActionClasses,
  cardBackgroundClasses,
  cardBaseClasses,
  cardContentClasses,
  cardFooterClasses,
  cardHeaderClasses,
  cardInteractiveClasses,
} from '@/components/ui/card.classes';

const BACKGROUNDS: ReadonlyArray<CardBackground> = [
  'none',
  'muted',
  'accent',
  'card',
  'primary',
  'secondary',
];

function parseBackground(value: string | null): CardBackground {
  return (BACKGROUNDS as ReadonlyArray<string>).includes(value ?? '')
    ? (value as CardBackground)
    : 'card';
}

/**
 * Compose the root .card node's class string from the shared class maps.
 * Exported so tests assert the WC renders the exact same composition the
 * React/Astro targets do -- the parity guarantee.
 */
export function composeCardClasses(background: CardBackground, interactive: boolean): string {
  return [
    cardBaseClasses,
    cardBackgroundClasses[background],
    interactive ? cardInteractiveClasses : '',
  ]
    .filter((part) => part.length > 0)
    .join(' ');
}

export class RaftersCard extends RaftersElement {
  static observedAttributes = ['interactive', 'background'];

  /**
   * The only component-owned CSS: the structural host-display shim. Custom
   * elements default to display:inline; the card wants the host to be a block.
   */
  static override styles = ':host { display: block; }';

  constructor() {
    super();
    this.addEventListener('keydown', this.handleKeydown);
  }

  override attributeChangedCallback(
    _name: string,
    oldValue: string | null,
    newValue: string | null,
  ): void {
    if (oldValue === newValue) return;
    this.applyInteractiveDom();
    this.update();
  }

  override connectedCallback(): void {
    super.connectedCallback();
    this.applyInteractiveDom();
  }

  private applyInteractiveDom(): void {
    if (this.hasAttribute('interactive')) {
      if (!this.hasAttribute('tabindex')) this.setAttribute('tabindex', '0');
      if (!this.hasAttribute('role')) this.setAttribute('role', 'button');
    } else {
      if (this.getAttribute('tabindex') === '0') this.removeAttribute('tabindex');
      if (this.getAttribute('role') === 'button') this.removeAttribute('role');
    }
  }

  private handleKeydown = (event: KeyboardEvent): void => {
    if (!this.hasAttribute('interactive')) return;
    if (event.key !== 'Enter' && event.key !== ' ') return;
    event.preventDefault();
    this.dispatchEvent(new CustomEvent('rafters-card-activate', { bubbles: true, composed: true }));
  };

  override render(): Node {
    const root = document.createElement('div');
    root.className = composeCardClasses(
      parseBackground(this.getAttribute('background')),
      this.hasAttribute('interactive'),
    );

    const header = document.createElement('div');
    header.className = cardHeaderClasses;
    const headerSlot = document.createElement('slot');
    headerSlot.setAttribute('name', 'header');
    header.appendChild(headerSlot);

    const action = document.createElement('div');
    action.className = cardActionClasses;
    const actionSlot = document.createElement('slot');
    actionSlot.setAttribute('name', 'action');
    action.appendChild(actionSlot);

    const content = document.createElement('div');
    content.className = cardContentClasses;
    const contentSlot = document.createElement('slot');
    contentSlot.setAttribute('name', 'content');
    content.appendChild(contentSlot);

    const footer = document.createElement('div');
    footer.className = cardFooterClasses;
    const footerSlot = document.createElement('slot');
    footerSlot.setAttribute('name', 'footer');
    footer.appendChild(footerSlot);

    const defaultSlot = document.createElement('slot');

    root.appendChild(header);
    root.appendChild(action);
    root.appendChild(content);
    root.appendChild(footer);
    root.appendChild(defaultSlot);

    return root;
  }
}

if (!customElements.get('rafters-card')) {
  customElements.define('rafters-card', RaftersCard);
}
