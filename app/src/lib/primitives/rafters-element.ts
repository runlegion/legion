/**
 * RaftersElement -- Base class for Rafters Web Components
 *
 * Extends HTMLElement with token-aware shadow DOM setup.
 * Each component gets a scoped style block resolved from DTCG tokens.
 * No Tailwind dependency in the shadow root.
 *
 * SECURITY NOTE: render() output is component-authored HTML, not user input.
 * Component authors control the template. User data must be escaped via
 * textContent or attribute binding, never interpolated into render() HTML.
 *
 * Usage:
 * ```typescript
 * class MyCard extends RaftersElement {
 *   static styles = `
 *     :host { display: block; }
 *     .card { background: var(--color-card); }
 *   `;
 *
 *   render() {
 *     return `<div class="card"><slot></slot></div>`;
 *   }
 * }
 * customElements.define('my-card', MyCard);
 * ```
 */

/**
 * Shared constructable stylesheet for base token custom properties.
 * Created once, adopted by all RaftersElement instances.
 * Set via RaftersElement.setTokenCSS() at app initialization.
 */
let sharedTokenSheet: CSSStyleSheet | null = null;

/**
 * Shared constructable stylesheet for the compiled utility classes
 * (the design system's Tailwind output). Created once, adopted by every
 * RaftersElement instance, so inner shadow markup can carry the SAME
 * utility class strings the React/Astro targets use instead of a
 * per-component hand-written CSS map.
 *
 * Token *values* reach the shadow root by custom-property inheritance from
 * the host `:root`; utility *rules* do not inherit, so the rule set must be
 * physically present in each root. This shared sheet is that presence --
 * parsed once, referenced by all roots.
 *
 * Set via RaftersElement.setUtilityCSS() at app initialization.
 */
let sharedUtilitySheet: CSSStyleSheet | null = null;

export class RaftersElement extends HTMLElement {
  /**
   * Component-specific CSS. Override in subclasses.
   * This CSS is scoped to the shadow root.
   */
  static styles = '';

  /**
   * Set the shared token stylesheet that all components adopt.
   * Call once at app initialization with the resolved DTCG token CSS.
   *
   * ```typescript
   * RaftersElement.setTokenCSS(`
   *   :host {
   *     --color-primary: oklch(0.208 0.042 266);
   *     --spacing-4: 1rem;
   *   }
   * `);
   * ```
   */
  static setTokenCSS(css: string): void {
    sharedTokenSheet = new CSSStyleSheet();
    sharedTokenSheet.replaceSync(css);
  }

  /**
   * Set the shared utility stylesheet that all components adopt.
   * Call once at app initialization with the design system's compiled
   * Tailwind output. Without it, components that style via utility classes
   * render unstyled (the class strings are present but no rules match).
   *
   * ```typescript
   * RaftersElement.setUtilityCSS(compiledRaftersCss);
   * ```
   */
  static setUtilityCSS(css: string): void {
    sharedUtilitySheet = new CSSStyleSheet();
    sharedUtilitySheet.replaceSync(css);
  }

  private _componentSheet: CSSStyleSheet | null = null;

  constructor() {
    super();
    this.attachShadow({ mode: 'open' });
  }

  connectedCallback(): void {
    const sheets: CSSStyleSheet[] = [];

    if (sharedTokenSheet) {
      sheets.push(sharedTokenSheet);
    }

    if (sharedUtilitySheet) {
      sheets.push(sharedUtilitySheet);
    }

    const ctor = this.constructor as typeof RaftersElement;
    if (ctor.styles) {
      this._componentSheet = new CSSStyleSheet();
      this._componentSheet.replaceSync(ctor.styles);
      sheets.push(this._componentSheet);
    }

    if (this.shadowRoot) {
      this.shadowRoot.adoptedStyleSheets = sheets;
      this.update();
    }
  }

  attributeChangedCallback(_name: string, oldValue: string | null, newValue: string | null): void {
    if (oldValue !== newValue) {
      this.update();
    }
  }

  disconnectedCallback(): void {
    this._componentSheet = null;
  }

  /**
   * Update the shadow DOM content.
   * Creates elements using DOM APIs instead of innerHTML for safety.
   * Override render() to return a DocumentFragment or Element.
   */
  update(): void {
    if (!this.shadowRoot) return;
    const content = this.render();
    this.shadowRoot.replaceChildren(content);
  }

  /**
   * Override in subclasses to return the component DOM.
   * Use document.createElement and friends, not HTML strings.
   */
  render(): Node {
    const slot = document.createElement('slot');
    return slot;
  }

  /**
   * Helper to read an attribute with a fallback.
   */
  protected attr(name: string, fallback = ''): string {
    return this.getAttribute(name) ?? fallback;
  }

  /**
   * Helper to check if an attribute is present (boolean attribute).
   */
  protected hasAttr(name: string): boolean {
    return this.hasAttribute(name);
  }

  /**
   * Helper to create an element with classes and optional text content.
   */
  protected el(tag: string, classes: string, text?: string): HTMLElement {
    const element = document.createElement(tag);
    if (classes) element.className = classes;
    if (text) element.textContent = text;
    return element;
  }
}
