/**
 * Typography primitives for consistent text styling and hierarchy
 *
 * @cognitive-load 2/10 - Familiar text patterns with clear visual hierarchy
 * @attention-economics Hierarchy guides reading: H1=page title (one per page), H2=sections, H3=subsections, body text flows naturally
 * @trust-building Consistent typography builds readability and professionalism
 * @accessibility Proper heading hierarchy for screen readers; sufficient contrast ratios
 * @semantic-meaning Element mapping: H1-H6=document structure, P=body content (with token overrides for lead/muted variants), Code=technical content
 *
 * @usage-patterns
 * DO: Use H1 once per page for main title
 * DO: Follow heading hierarchy (H1 -> H2 -> H3, never skip)
 * DO: Use P with size/color token props for variant styling (e.g., lead, muted)
 * DO: Use token props to override individual typography dimensions
 * NEVER: Skip heading levels (H1 -> H3)
 * NEVER: Use headings for styling only (use CSS instead)
 * NEVER: Use multiple H1s on a single page
 *
 * @example
 * ```tsx
 * // Page structure
 * <H1>Page Title</H1>
 * <P size="xl" color="muted">This is an introduction to the page content.</P>
 *
 * <H2>Section Title</H2>
 * <P>Body paragraph with standard styling.</P>
 *
 * <H3>Subsection</H3>
 * <P>More content here.</P>
 * <P size="sm" color="muted">Last updated: Jan 2025</P>
 *
 * // Code example
 * <P>Use the <Code>useState</Code> hook for local state.</P>
 *
 * // Blockquote
 * <Blockquote>
 *   "Design is not just what it looks like. Design is how it works."
 * </Blockquote>
 * ```
 */
import * as React from 'react';
import classy from '@/lib/primitives/classy';
import {
  resolveVariant,
  variantForElement,
  variantToTag,
  type TypographyElement,
  type TypographyTokenProps,
  type TypographyVariant,
} from '@/components/ui/typography.behavior';
import { resolveTypography } from '@/components/ui/typography.classes';

/**
 * The React performances of the static Typography score. A static has nothing
 * to subscribe to -- no useMemory, no controller, no composed primitives: token
 * props in, resolved classes out, the semantic tag chosen by the variant. Each
 * named component is a thin wrapper the factory builds; the only two things
 * added around the score are the view (typography.classes.ts) and the tag.
 */

export type {
  TypographyElement,
  TypographyTokenProps,
  TypographyVariant,
} from '@/components/ui/typography.behavior';

export interface TypographyComponentProps
  extends React.HTMLAttributes<HTMLElement>, TypographyTokenProps {}

interface FactoryOptions {
  /** codeblock nests its children in a <code> inside the <pre> root. */
  nestCode?: boolean;
}

function splitTokenProps(props: TypographyComponentProps): {
  tokens: TypographyTokenProps;
  rest: Omit<TypographyComponentProps, keyof TypographyTokenProps>;
} {
  const { size, weight, color, line, tracking, family, align, transform, ...rest } = props;
  return { tokens: { size, weight, color, line, tracking, family, align, transform }, rest };
}

/**
 * Build a named typography component for a fixed variant + tag. This is DRY
 * construction, not a decision surface -- every variant runs the same
 * resolveTypography path, so the wrapper stays thin.
 */
function makeTypography(
  variant: TypographyVariant,
  tag: string,
  displayName: string,
  options: FactoryOptions = {},
): React.ForwardRefExoticComponent<TypographyComponentProps & React.RefAttributes<HTMLElement>> {
  const Component = React.forwardRef<HTMLElement, TypographyComponentProps>((props, ref) => {
    const { tokens, rest } = splitTokenProps(props);
    const { className, children, ...attrs } = rest;
    const classes = classy(resolveTypography(variant, tokens), className);
    const content = options.nestCode ? React.createElement('code', null, children) : children;
    return React.createElement(
      tag,
      { ref, 'data-part': 'root', className: classes || undefined, ...attrs },
      content,
    );
  });
  Component.displayName = displayName;
  return Component;
}

// Headings. h5/h6 render their own tags but borrow h4's scale -- the variant
// vocabulary stops at h4, faithful to the oracle.
export const H1 = makeTypography('h1', 'h1', 'H1');
export const H2 = makeTypography('h2', 'h2', 'H2');
export const H3 = makeTypography('h3', 'h3', 'H3');
export const H4 = makeTypography('h4', 'h4', 'H4');
export const H5 = makeTypography('h4', 'h5', 'H5');
export const H6 = makeTypography('h4', 'h6', 'H6');

// Body and its presentational variants (all render <p>).
export const P = makeTypography('p', 'p', 'P');
export const Lead = makeTypography('lead', 'p', 'Lead');
export const Large = makeTypography('large', 'p', 'Large');
export const Muted = makeTypography('muted', 'p', 'Muted');

// Inline + block text.
export const Small = makeTypography('small', 'small', 'Small');
export const Code = makeTypography('code', 'code', 'Code');
export const CodeBlock = makeTypography('codeblock', 'pre', 'CodeBlock', { nestCode: true });
export const Blockquote = makeTypography('blockquote', 'blockquote', 'Blockquote');
export const Mark = makeTypography('mark', 'mark', 'Mark');
export const Abbr = makeTypography('abbr', 'abbr', 'Abbr');

// Lists.
export const Ul = makeTypography('ul', 'ul', 'Ul');
export const Ol = makeTypography('ol', 'ol', 'Ol');
export const Li = makeTypography('li', 'li', 'Li');
/** shadcn parity alias: the default list is an unordered list. */
export const List = Ul;

export interface TypographyProps extends TypographyComponentProps {
  /** Native element to render. Drives the variant when `variant` is absent. */
  as?: TypographyElement;
  /** Preset. Overrides the element-derived variant. */
  variant?: TypographyVariant;
}

/**
 * The generic wrapper: an escape hatch for dynamic element selection. `as`
 * chooses the tag and derives the variant (h5/h6 borrow h4; span reads as
 * body); an explicit `variant` overrides that derivation.
 *
 * @cognitive-load 1/10 - decision 0, information 1, interaction 0, disruption
 * 0, learning 0. Reading text is not a decision and not an interaction; the
 * only load is parsing the words themselves. Consistent hierarchy makes even
 * that information cost lower, not higher. A static, universally learned
 * surface with zero workflow disruption.
 * @attention-economics Hierarchy IS the attention budget: one H1 names the
 * page, H2/H3 rank the sections, body text recedes, muted/small step further
 * back. The scale spends contrast where attention should land and withholds it
 * everywhere else, so a scan resolves structure before a single word is read.
 * @trust-building Consistent type across every element and framework reads as
 * professional and considered; a stable heading hierarchy makes long content
 * feel navigable and honest rather than arbitrary. Nothing shifts, nothing
 * surprises -- the text set is furniture the reader stops noticing.
 * @accessibility The semantic element IS the contract: headings are real
 * h1-h6 (screen-reader document outline, never headings-for-styling),
 * blockquote is a real quotation, lists are real lists, code/mark/abbr carry
 * native semantics. No ARIA is projected because none is needed; authors own
 * heading order (never skip a level) and sufficient token-driven contrast.
 */
export const Typography = React.forwardRef<HTMLElement, TypographyProps>((props, ref) => {
  const { as: element = 'p', variant, ...componentProps } = props;
  const { tokens, rest } = splitTokenProps(componentProps);
  const { className, children, ...attrs } = rest;
  const resolvedVariant = variant ? resolveVariant(variant) : variantForElement(element);
  const classes = classy(resolveTypography(resolvedVariant, tokens), className);
  return React.createElement(
    element,
    { ref, 'data-part': 'root', className: classes || undefined, ...attrs },
    children,
  );
});

Typography.displayName = 'Typography';

// Re-exported for callers that build the class string directly (parity guard).
export { resolveTypography, typographyClasses } from '@/components/ui/typography.classes';
export { variantToTag };

export default Typography;
