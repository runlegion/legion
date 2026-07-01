/**
 * Shared card class definitions
 *
 * Imported by both card.tsx (React) and card.astro (Astro)
 * to ensure visual parity across framework targets.
 */

export const cardBaseClasses =
  'bg-card text-card-foreground border border-card-border rounded-lg shadow-sm';

export const cardInteractiveClasses =
  'hover:bg-card-hover hover:shadow-md transition-shadow duration-150 motion-reduce:transition-none focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-2';

export const cardEditableClasses =
  'outline-2 outline-dashed outline-muted-foreground/30 outline-offset-2';

export const cardHeaderClasses = 'flex flex-col gap-1.5 p-6';
export const cardHeaderFlushClasses =
  'flex flex-col gap-1.5 p-0 [&_img]:w-full [&_img]:object-cover';

export const cardTitleClasses = 'text-title-medium leading-none';

export const cardDescriptionClasses = 'text-body-small text-muted-foreground';

export const cardActionClasses = 'col-start-2 row-span-2 row-start-1 self-start justify-self-end';

export const cardContentClasses = 'p-6 pt-0';

export const cardFooterClasses = 'flex items-center p-6 pt-0';

export const cardEditableFocusClasses =
  'outline-none focus:ring-2 focus:ring-ring focus:ring-offset-2 rounded';

export type CardBackground = 'none' | 'muted' | 'accent' | 'card' | 'primary' | 'secondary';

export const cardBackgroundClasses: Record<CardBackground, string> = {
  none: '',
  muted: 'bg-muted text-muted-foreground',
  accent: 'bg-accent text-accent-foreground',
  card: 'bg-card text-card-foreground',
  primary: 'bg-primary text-primary-foreground',
  secondary: 'bg-secondary text-secondary-foreground',
};
