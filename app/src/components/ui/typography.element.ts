/**
 * <rafters-typography> -- token-aware typography Web Component
 *
 * Framework-target for the Typography component, parallel to typography.tsx
 * (React) and the typography.astro family. Renders any of 17 variants (h1-h4,
 * p, lead, large, small, muted, code, blockquote, ul, ol, li, codeblock, mark,
 * abbr) inside shadow DOM. The inner semantic tag carries the SAME utility
 * class strings the React/Astro targets use -- composed via resolveTypography
 * from typography.classes.ts -- rather than a parallel hand-written CSS map.
 * Presentation resolves from the shared compiled utility sheet adopted by
 * RaftersElement (setUtilityCSS) plus the token custom properties inherited
 * from the host :root.
 *
 * The variant attribute drives both the rendered tag AND the composed class
 * string. TypographyTokenProps attributes
 * (size/weight/color/line/tracking/family/align/transform) override the
 * variant defaults at compose time.
 *
 * The only shadow-scoped CSS this component owns is the structural :host
 * display shim. The inner element's own block/inline nature (and any display
 * utility on it) governs its layout.
 *
 * Auto-registers as 'rafters-typography' on import. Registration is
 * idempotent. Unknown variants silently fall back to 'p' -- NEVER throws.
 *
 * DOM is built via document.createElement / appendChild. No innerHTML.
 */

import { RaftersElement } from '@/lib/primitives/rafters-element';
import {
  resolveTypography,
  type TypographyTokenProps,
  type TypographyVariant,
} from '@/components/ui/typography.classes';

export type { TypographyTokenProps, TypographyVariant } from '@/components/ui/typography.classes';

// ============================================================================
// Variant -> tag and variant coercion (previously in typography.styles.ts)
// ============================================================================

/**
 * Variant -> semantic HTML tag (the element rendered inside shadow DOM).
 * Unknown variants fall back to 'p' via resolveVariant -- NEVER throw.
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
  blockquote: 'blockquote',
  ul: 'ul',
  ol: 'ol',
  li: 'li',
  codeblock: 'pre',
  mark: 'mark',
  abbr: 'abbr',
};

/**
 * Coerce an arbitrary value to a known variant. Unknown values fall back to
 * 'p'. NEVER throws.
 */
export function resolveVariant(value: unknown): TypographyVariant {
  if (typeof value !== 'string' || value.length === 0) return 'p';
  if (value in variantToTag) return value as TypographyVariant;
  return 'p';
}

// ============================================================================
// Observed Attributes
// ============================================================================

/** Attribute names that map to TypographyTokenProps keys. */
const OVERRIDE_ATTRIBUTES = [
  'size',
  'weight',
  'color',
  'line',
  'tracking',
  'family',
  'align',
  'transform',
] as const;

type OverrideAttribute = (typeof OVERRIDE_ATTRIBUTES)[number];

/** All attributes the element observes. Variant is first; overrides follow. */
const OBSERVED_ATTRIBUTES = ['variant', ...OVERRIDE_ATTRIBUTES] as const;

/**
 * Compose the inner element's class string from the shared resolver.
 * Exported so tests assert the WC renders the exact same composition the
 * Astro target does -- the parity guarantee.
 */
export function composeTypographyClasses(
  variant: TypographyVariant,
  overrides: TypographyTokenProps = {},
): string {
  return resolveTypography(variant, overrides);
}

// ============================================================================
// Component
// ============================================================================

export class RaftersTypography extends RaftersElement {
  static override styles = ':host { display: block; }';

  static readonly observedAttributes: readonly string[] = OBSERVED_ATTRIBUTES;

  /**
   * Read all TypographyTokenProps attributes off the element, omitting absent
   * entries so the resolver skips them cleanly.
   */
  private readOverrides(): TypographyTokenProps {
    const out: TypographyTokenProps = {};
    for (const attr of OVERRIDE_ATTRIBUTES) {
      const value = this.getAttribute(attr);
      if (value !== null && value.length > 0) {
        out[attr satisfies OverrideAttribute] = value;
      }
    }
    return out;
  }

  /** Resolve the current variant from the `variant` attribute. */
  private currentVariant(): TypographyVariant {
    return resolveVariant(this.getAttribute('variant'));
  }

  /**
   * Build the semantic tag tree for the current variant, carrying the
   * composed utility class string.
   * codeblock -> pre > code > slot
   * All other variants -> tag > slot
   */
  override render(): Node {
    const variant = this.currentVariant();
    const tag = variantToTag[variant];
    const className = composeTypographyClasses(variant, this.readOverrides());
    const root = document.createElement(tag);
    root.className = className;

    if (variant === 'codeblock') {
      const code = document.createElement('code');
      code.appendChild(document.createElement('slot'));
      root.appendChild(code);
      return root;
    }

    root.appendChild(document.createElement('slot'));
    return root;
  }
}

// ============================================================================
// Auto-registration (idempotent)
// ============================================================================

const TAG_NAME = 'rafters-typography';
if (typeof customElements !== 'undefined' && !customElements.get(TAG_NAME)) {
  customElements.define(TAG_NAME, RaftersTypography);
}
