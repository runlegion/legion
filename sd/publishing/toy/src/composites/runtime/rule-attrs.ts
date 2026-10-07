/**
 * Pure rule -> native HTML5 constraint-attribute mapper
 *
 * A block carries validation intent as `AppliedRule[]` (see manifest.ts). At
 * build time the render engine turns those rules into native HTML5 constraint
 * ATTRIBUTES on the emitted element -- `required`, `type="email"`, `pattern`,
 * `minlength`, etc. There is no client JS, no installed validator code, and no
 * runtime rule artifact: the browser's own constraint validation does the work.
 *
 * This module is pure and browser-safe (NO node:fs). It is the single decision
 * point for "what attributes does this rule become", so it can be exhaustively
 * unit-tested in isolation and imported from both the package root and the
 * browser-safe `client` entry.
 *
 * Built-in rule names map to fixed attribute sets. Parameterized rules carry a
 * `config` object whose values drive the attribute (e.g. `pattern`, custom
 * `minlength`/`maxlength`). Unknown rule names contribute nothing -- they are
 * silently ignored so an unrecognized rule never breaks the build or emits a
 * bogus attribute.
 */

import type { AppliedRule } from './manifest';

/**
 * Fixed native-attribute sets for the built-in string rules. Keyed by the rule
 * NAME only (the zod bodies live in built-in-rules/ and are never needed here).
 * `credentials` is a composite-level concern (it pairs two fields) and has no
 * single-element attribute mapping, so it is deliberately absent.
 */
const STRING_RULE_ATTRS: Record<string, Record<string, unknown>> = {
  required: { required: true },
  email: { type: 'email' },
  url: { type: 'url' },
  password: { type: 'password', minlength: 8 },
};

/** Read a numeric `config` value, accepting `value` as a string of digits. */
function numericConfig(config: Record<string, unknown>, key: string): number | null {
  const raw = config[key];
  if (typeof raw === 'number' && Number.isFinite(raw)) return raw;
  if (typeof raw === 'string' && raw.trim() !== '' && Number.isFinite(Number(raw))) {
    return Number(raw);
  }
  return null;
}

/**
 * Attributes for a single parameterized rule (`{ name, config }`). Unknown
 * names or malformed config yield `{}` and are ignored by the caller.
 */
function parameterizedAttrs(
  name: string,
  config: Record<string, unknown>,
): Record<string, unknown> {
  switch (name) {
    case 'pattern': {
      const pattern = config.pattern;
      return typeof pattern === 'string' ? { pattern } : {};
    }
    case 'minlength': {
      const value = numericConfig(config, 'value');
      return value === null ? {} : { minlength: value };
    }
    case 'maxlength': {
      const value = numericConfig(config, 'value');
      return value === null ? {} : { maxlength: value };
    }
    default: {
      // A built-in string rule may also arrive in object form -- honor it.
      return STRING_RULE_ATTRS[name] ?? {};
    }
  }
}

/**
 * Map a block's `rules` to the native HTML5 constraint attributes it should
 * emit. Pure -- the same input always yields the same output, with no I/O.
 *
 * - `undefined` (no rules) -> `{}`.
 * - A string rule -> its fixed built-in attribute set, or `{}` if unknown.
 * - A `{ name, config }` rule -> its parameterized attributes (`pattern`,
 *   `minlength`, `maxlength`), or a built-in set if the name is a string rule.
 *
 * Later rules win on attribute conflicts (e.g. an explicit `minlength` after
 * `password` overrides the password default).
 */
export function rulesToHtmlAttrs(rules: AppliedRule[] | undefined): Record<string, unknown> {
  if (!rules) return {};

  const attrs: Record<string, unknown> = {};
  for (const rule of rules) {
    const next =
      typeof rule === 'string'
        ? (STRING_RULE_ATTRS[rule] ?? {})
        : parameterizedAttrs(rule.name, rule.config);
    Object.assign(attrs, next);
  }
  return attrs;
}
