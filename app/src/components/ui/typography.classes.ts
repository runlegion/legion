/**
 * Shared class resolution for Typography components.
 *
 * Variant defaults are stored dimensionally (size, weight, color, line, tracking,
 * family, align, transform) rather than as flat utility strings. Token prop
 * overrides replace the matching dimension at emit time, so defaults never fight
 * overrides in the Tailwind cascade (which orders utilities alphabetically and
 * thus cannot be trusted for overrides: `text-accent` would lose to
 * `text-foreground` on 'a' < 'f').
 */

import { resolveFillName } from '@/lib/primitives/fill-resolver';

export type TypographyVariant =
  | 'h1'
  | 'h2'
  | 'h3'
  | 'h4'
  | 'p'
  | 'lead'
  | 'large'
  | 'small'
  | 'muted'
  | 'code'
  | 'codeblock'
  | 'blockquote'
  | 'mark'
  | 'abbr'
  | 'ul'
  | 'ol'
  | 'li';

export interface TypographyTokenProps {
  size?: string | undefined;
  weight?: string | undefined;
  color?: string | undefined;
  line?: string | undefined;
  tracking?: string | undefined;
  family?: string | undefined;
  align?: string | undefined;
  transform?: string | undefined;
}

interface CqOverrides {
  size?: string;
  weight?: string;
  line?: string;
  tracking?: string;
}

interface VariantDefaults extends TypographyTokenProps {
  layout?: string;
  cq?: Record<string, CqOverrides>;
}

const VARIANTS: Record<TypographyVariant, VariantDefaults> = {
  h1: {
    size: '4xl',
    weight: 'bold',
    tracking: 'tight',
    color: 'foreground',
    layout: 'scroll-m-20',
    cq: { lg: { size: '5xl' } },
  },
  h2: {
    size: '3xl',
    weight: 'semibold',
    tracking: 'tight',
    color: 'foreground',
    layout: 'scroll-m-20',
  },
  h3: {
    size: '2xl',
    weight: 'semibold',
    tracking: 'tight',
    color: 'foreground',
    layout: 'scroll-m-20',
  },
  h4: {
    size: 'xl',
    weight: 'semibold',
    tracking: 'tight',
    color: 'foreground',
    layout: 'scroll-m-20',
  },
  p: { line: '7', color: 'foreground' },
  lead: { size: 'xl', color: 'muted-foreground' },
  large: { size: 'lg', weight: 'semibold', color: 'foreground' },
  small: { size: 'sm', weight: 'medium', line: 'none', color: 'foreground' },
  muted: { size: 'sm', color: 'muted-foreground' },
  code: {
    size: 'sm',
    family: 'mono',
    color: 'foreground',
    layout: 'rounded bg-muted px-1 py-0.5',
  },
  codeblock: {
    size: 'sm',
    family: 'mono',
    color: 'foreground',
    layout: 'relative rounded-lg bg-muted p-4 overflow-x-auto [&_code]:bg-transparent [&_code]:p-0',
  },
  blockquote: { color: 'foreground', layout: 'mt-6 border-l-2 border-border pl-6 italic' },
  mark: { color: 'accent-foreground', layout: 'bg-accent px-1 rounded' },
  abbr: { layout: 'cursor-help underline decoration-dotted underline-offset-4' },
  ul: { color: 'foreground', layout: 'my-6 ml-6 list-disc [&>li]:mt-2' },
  ol: { color: 'foreground', layout: 'my-6 ml-6 list-decimal [&>li]:mt-2' },
  li: { line: '7' },
};

const DIM_TO_UTIL: Record<keyof TypographyTokenProps, (v: string) => string> = {
  size: (v) => `text-${v}`,
  weight: (v) => `font-${v}`,
  line: (v) => `leading-${v}`,
  tracking: (v) => `tracking-${v}`,
  family: (v) => `font-${v}`,
  align: (v) => `text-${v}`,
  transform: (v) => v,
  // Fill signature in text context: plain words emit text-{word}; a
  // word-to-word signature emits gradient text via bg-clip-text. Invalid
  // signatures emit nothing -- same contract as Container/Card (#1637).
  color: (v) => resolveFillName(v, 'text'),
};

const DIM_KEYS = Object.keys(DIM_TO_UTIL) as (keyof TypographyTokenProps)[];

function emitDim(dim: keyof TypographyTokenProps, value: string, prefix = ''): string {
  const util = DIM_TO_UTIL[dim](value);
  if (!util || !prefix) return util;
  return util
    .split(/\s+/)
    .map((u) => `${prefix}${u}`)
    .join(' ');
}

function emitProps(props: TypographyTokenProps, prefix = ''): string {
  const classes: string[] = [];
  for (const key of DIM_KEYS) {
    const value = props[key];
    if (value == null) continue;
    const util = emitDim(key, value, prefix);
    if (util) classes.push(util);
  }
  return classes.join(' ');
}

export function resolveTypography(
  variant: TypographyVariant,
  overrides: TypographyTokenProps = {},
): string {
  const defaults = VARIANTS[variant];
  const merged: TypographyTokenProps = {};
  for (const key of DIM_KEYS) merged[key] = overrides[key] ?? defaults[key];

  const parts: string[] = [];
  if (defaults.layout) parts.push(defaults.layout);
  parts.push(emitProps(merged));

  // CQ defaults survive only where the prop didn't override the same dimension.
  if (defaults.cq) {
    for (const [breakpoint, cqDefaults] of Object.entries(defaults.cq)) {
      const surviving: TypographyTokenProps = {};
      for (const key of Object.keys(cqDefaults) as (keyof CqOverrides)[]) {
        if (overrides[key] == null) surviving[key] = cqDefaults[key];
      }
      parts.push(emitProps(surviving, `@${breakpoint}:`));
    }
  }

  return parts.filter(Boolean).join(' ');
}

/**
 * Emit token-prop classes without any variant defaults. Exposed for tests that
 * exercise the prop-to-utility emit path in isolation (see typography-fill.test).
 */
export function tokenPropsToClasses(props: TypographyTokenProps): string {
  return emitProps(props);
}

/**
 * Flat-string map of variant defaults with no overrides. Kept for consumers
 * that need the baseline class string (e.g. List's li, CodeBlock wrapper).
 */
export const typographyClasses = Object.fromEntries(
  (Object.keys(VARIANTS) as TypographyVariant[]).map((v) => [v, resolveTypography(v)]),
) as Record<TypographyVariant, string>;
