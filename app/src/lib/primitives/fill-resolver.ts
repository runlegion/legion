/**
 * Fill Resolver Primitive (v2, #1637)
 *
 * A fill is a SIGNATURE: a compact form that sets namespaced classes and
 * existing Tailwind utilities on an element. Color vocabulary only:
 *
 *   word            solid            fill="primary"
 *   word/alpha      with opacity     fill="muted/50"     (Tailwind's slash, verbatim)
 *   word-to-word    2-stop gradient  fill="primary-to-primary/0"
 *
 * '-to-' is reserved in the color namer, so the first '-to-' split is
 * deterministic. Expansion is optimistic at runtime (components never
 * crash); the build-time safelist pass validates words against the
 * registry vocabulary and is the strict gate.
 *
 * Foreground pairing is membership in the frozen role contract, never
 * spelling -- family words and unknown words never emit a phantom
 * text-*-foreground class.
 *
 * This module mirrors the canonical implementation in
 * @rafters/shared/src/fill-signature.ts -- ui stays dependency-free
 * because it is copied into consumer projects via the registry. A parity
 * test in packages/ui/test keeps the two in lockstep.
 *
 * Zero external dependencies -- works in React, Astro, and Web Components.
 */

export type FillContext = 'surface' | 'text';

interface FillStop {
  word: string;
  alpha?: number;
}

const SEPARATOR = '-to-';
const ALPHA_PATTERN = /^(100|[1-9]?[0-9])$/;
const WORD_PATTERN = /^[a-z][a-z0-9]*(-[a-z0-9]+)*$/;

/** Mirror of PAIRED_SURFACE_ROLES in @rafters/shared fill-signature. */
const PAIRED_SURFACE_ROLES = new Set([
  'background',
  'card',
  'panel',
  'popover',
  'surface',
  'primary',
  'secondary',
  'muted',
  'accent',
  'destructive',
  'success',
  'warning',
  'info',
  'alert',
  'highlight',
  'selection',
  'sidebar',
  'nav',
  'tooltip',
  'overlay',
  'table',
  'table-header',
  'code',
  'badge',
  'avatar',
  'input',
]);

function parseStop(term: string): FillStop | null {
  const slash = term.indexOf('/');
  const word = slash === -1 ? term : term.slice(0, slash);
  if (!WORD_PATTERN.test(word)) return null;
  if (slash === -1) return { word };
  const alphaRaw = term.slice(slash + 1);
  if (!ALPHA_PATTERN.test(alphaRaw)) return null;
  return { word, alpha: Number(alphaRaw) };
}

function parseSignature(input: string): [FillStop] | [FillStop, FillStop] | null {
  const trimmed = input.trim();
  if (trimmed.length === 0 || trimmed.includes(' ')) return null;

  const split = trimmed.indexOf(SEPARATOR);
  if (split === -1) {
    const stop = parseStop(trimmed);
    return stop ? [stop] : null;
  }

  const right = trimmed.slice(split + SEPARATOR.length);
  if (right.includes(SEPARATOR)) return null; // two stops only
  const from = parseStop(trimmed.slice(0, split));
  const to = parseStop(right);
  return from && to ? [from, to] : null;
}

/** Membership in the frozen role contract decides foreground pairing. */
function foregroundWordFor(word: string): string | null {
  if (!PAIRED_SURFACE_ROLES.has(word)) return null;
  return word === 'background' ? 'foreground' : `${word}-foreground`;
}

function stopClass(prefix: string, stop: FillStop): string {
  return stop.alpha === undefined
    ? `${prefix}-${stop.word}`
    : `${prefix}-${stop.word}/${stop.alpha}`;
}

/**
 * Resolve a fill signature into CSS classes for a given context.
 *
 * Surface: bg word (+ paired foreground for frozen role words); gradients
 * emit Tailwind v4 utilities (bg-linear-to-b -- never the deprecated
 * bg-gradient-to-*). Text: text word, or gradient text via bg-clip-text.
 *
 * Dark behavior falls out of which utility is emitted: semantic words
 * compile to utilities over flipping vars, family words to literal scale
 * utilities that never flip.
 *
 * Invalid signatures return '' -- runtime never crashes; the build-time
 * safelist pass rejects them naming the unresolvable word.
 */
export function resolveFillName(name: string | undefined, context: FillContext): string {
  if (!name) return '';

  const stops = parseSignature(name);
  if (!stops) return '';

  const [first, second] = stops;

  if (!second) {
    if (context === 'text') return stopClass('text', first);
    const parts = [stopClass('bg', first)];
    const fg = foregroundWordFor(first.word);
    if (fg) parts.push(`text-${fg}`);
    return parts.join(' ');
  }

  const gradient = ['bg-linear-to-b', stopClass('from', first), stopClass('to', second)];
  if (context === 'text') {
    return [...gradient, 'bg-clip-text', 'text-transparent'].join(' ');
  }
  const fg = foregroundWordFor(first.word);
  if (fg) gradient.push(`text-${fg}`);
  return gradient.join(' ');
}
