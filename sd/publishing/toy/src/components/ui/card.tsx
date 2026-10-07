/**
 * Flexible container component for grouping related content with semantic structure
 *
 * @cognitive-load 2/10 - Simple container with clear boundaries and minimal cognitive overhead
 * @attention-economics Neutral container: Content drives attention, elevation hierarchy for interactive states
 * @trust-building Consistent spacing, predictable interaction patterns, clear content boundaries
 * @accessibility Proper heading structure, landmark roles, keyboard navigation for interactive cards
 * @semantic-meaning Structural roles: article=standalone content, section=grouped content, aside=supplementary information
 *
 * @usage-patterns
 * DO: Group related information with clear visual boundaries
 * DO: Create interactive cards with hover states and focus management
 * DO: Establish information hierarchy with header, content, actions
 * DO: Implement responsive scaling with consistent proportions
 * NEVER: Use decorative containers without semantic purpose
 * NEVER: Nest cards within cards
 * NEVER: Use Card for layout (use Grid/Container instead)
 *
 * @example
 * ```tsx
 * // Standalone content - use article
 * <Card as="article">
 *   <CardHeader>
 *     <CardTitle>Blog Post Title</CardTitle>
 *     <CardDescription>Published Jan 2025</CardDescription>
 *   </CardHeader>
 *   <CardContent>Post excerpt...</CardContent>
 * </Card>
 *
 * // Interactive card - product listing
 * <Card interactive>
 *   <CardHeader>
 *     <CardTitle>Product Name</CardTitle>
 *   </CardHeader>
 *   <CardContent>$99.00</CardContent>
 *   <CardFooter>
 *     <Button>Add to Cart</Button>
 *   </CardFooter>
 * </Card>
 *
 * // Supplementary content - use aside
 * <Card as="aside">
 *   <CardHeader>
 *     <CardTitle>Related Links</CardTitle>
 *   </CardHeader>
 *   <CardContent>...</CardContent>
 * </Card>
 * ```
 */
import * as React from 'react';
import type { CardConfig, CardElement } from '@/components/ui/card.behavior';
import {
  cardActionClasses,
  cardClasses,
  cardContentClasses,
  cardDescriptionClasses,
  cardFooterClasses,
  cardHeaderClasses,
  cardTitleClasses,
} from '@/components/ui/card.classes';

/**
 * `className` is NOT part of any Card surface -- see the "no className" section
 * of docs/spec/components/card.md. Design travels through token props (`fill`,
 * `as`); a class escape hatch would let a caller re-do design ad hoc, which is
 * the one thing this system exists to prevent. Every props type below Omits it
 * from the HTML attributes it extends, and every performance destructures it
 * away so a spread cannot smuggle it onto the element.
 */
type NoClassName<T> = Omit<T, 'className'>;

/**
 * Runtime half of the same rule. The type Omit stops a TypeScript caller; this
 * stops a JavaScript one, and stops `...props` from carrying a `className` the
 * type system never saw onto the element.
 *
 * Returns a fresh object when it strips; no memo boundary depends on its
 * identity (the result is spread straight onto a DOM node).
 */
function withoutClassName<T extends object>(props: T): T {
  if (!('className' in props)) return props;
  const { className: _discarded, ...rest } = props as T & { className?: unknown };
  return rest as unknown as T;
}

export interface CardProps extends NoClassName<React.HTMLAttributes<HTMLDivElement>> {
  as?: CardElement;
  /**
   * Fill signature over the colour vocabulary (#1637): `word`, `word/alpha`,
   * or `word-to-word`. When it resolves, the fill surface replaces the
   * default `bg-card` surface; an invalid signature keeps the default.
   */
  fill?: string;
}

export const Card = React.forwardRef<HTMLDivElement, CardProps>(
  ({ as: Element = 'div', fill, children, ...rest }, ref) => {
    const config: CardConfig = { as: Element, fill };
    const classes = cardClasses(config, {});
    const props = withoutClassName(rest);

    // No effects and no optional parts -- nothing ever calls getPart, so the
    // ref is a plain forward. The score projects nothing, so there is no aria
    // to spread; the element's own semantics are the whole contract.
    return React.createElement(
      Element,
      {
        ref,
        'data-part': 'root',
        // data-slot is the SWAP contract (shadcn's selector surface, so a
        // consumer's has-data-[slot=card] rules keep matching); data-part stays
        // the internal binding contract. Root carries both; it is the only node
        // with a declared part.
        'data-slot': 'card',
        className: classes.root,
        'data-fill': fill || undefined,
        ...props,
      },
      children,
    );
  },
);

Card.displayName = 'Card';

export type CardHeaderProps = NoClassName<React.HTMLAttributes<HTMLDivElement>>;

/**
 * The header is a grid (shadcn v4), not a flex column: that is the parent
 * CardAction's placement utilities need in order to place at all.
 *
 * @parent card
 */
export const CardHeader = React.forwardRef<HTMLDivElement, CardHeaderProps>((props, ref) => (
  <div
    ref={ref}
    data-slot="card-header"
    className={cardHeaderClasses}
    {...withoutClassName(props)}
  />
));

CardHeader.displayName = 'CardHeader';

export interface CardTitleProps extends NoClassName<React.HTMLAttributes<HTMLHeadingElement>> {
  as?: 'h1' | 'h2' | 'h3' | 'h4' | 'h5' | 'h6';
}

/**
 * Renders a REAL heading (default h3), where shadcn renders a div. This is the
 * accepted AAA divergence: it is behavior-additive (a heading appears in the
 * document outline and in a screen reader's heading list, satisfying 1.3.1 and
 * 2.4.10 Section Headings at AAA) and invisible to a swap -- same component
 * name, same children, same data-slot, one added prop. `as` places the heading
 * at the correct outline level for the surrounding page. See card.md.
 *
 * @parent card
 */
export const CardTitle = React.forwardRef<HTMLHeadingElement, CardTitleProps>(
  ({ as: Element = 'h3', children, ...rest }, ref) =>
    React.createElement(
      Element,
      {
        ref,
        'data-slot': 'card-title',
        className: cardTitleClasses,
        ...withoutClassName(rest),
      },
      children,
    ),
);

CardTitle.displayName = 'CardTitle';

export type CardDescriptionProps = NoClassName<React.HTMLAttributes<HTMLParagraphElement>>;

/**
 * A real `p`, where shadcn renders a div -- the same accepted AAA divergence as
 * the title, and behavior-additive for the same reason: prose in a paragraph is
 * navigable and correctly announced, and the swap sees no API change.
 *
 * @parent card
 */
export const CardDescription = React.forwardRef<HTMLParagraphElement, CardDescriptionProps>(
  ({ children, ...rest }, ref) =>
    React.createElement(
      'p',
      {
        ref,
        'data-slot': 'card-description',
        className: cardDescriptionClasses,
        ...withoutClassName(rest),
      },
      children,
    ),
);

CardDescription.displayName = 'CardDescription';

export type CardActionProps = NoClassName<React.HTMLAttributes<HTMLDivElement>>;

/**
 * Trailing control slot (dismiss/menu). Its placement utilities only resolve as
 * a DIRECT CHILD of CardHeader -- the header's `has-data-[slot=card-action]`
 * variant is what opens the second column for it.
 *
 * @parent card
 */
export const CardAction = React.forwardRef<HTMLDivElement, CardActionProps>((props, ref) => (
  <div
    ref={ref}
    data-slot="card-action"
    className={cardActionClasses}
    {...withoutClassName(props)}
  />
));

CardAction.displayName = 'CardAction';

export type CardContentProps = NoClassName<React.HTMLAttributes<HTMLDivElement>>;

/** @parent card */
export const CardContent = React.forwardRef<HTMLDivElement, CardContentProps>((props, ref) => (
  <div
    ref={ref}
    data-slot="card-content"
    className={cardContentClasses}
    {...withoutClassName(props)}
  />
));

CardContent.displayName = 'CardContent';

export type CardFooterProps = NoClassName<React.HTMLAttributes<HTMLDivElement>>;

/** @parent card */
export const CardFooter = React.forwardRef<HTMLDivElement, CardFooterProps>((props, ref) => (
  <div
    ref={ref}
    data-slot="card-footer"
    className={cardFooterClasses}
    {...withoutClassName(props)}
  />
));

CardFooter.displayName = 'CardFooter';

export default Card;
