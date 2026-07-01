/**
 * Classy — Tailwind-aware class builder with token resolution
 *
 * Features:
 * - Full Tailwind syntax understanding (modifiers vs values)
 * - Token refs for design system integration
 * - Arbitrary value detection and blocking
 * - Class deduplication and normalization
 *
 * Tailwind class structure: [modifier:]...[modifier:]utility[-value]
 * - Modifiers can contain brackets: data-[state=open]:, aria-[checked]:, min-[320px]:
 * - Utility values with brackets are arbitrary: w-[100px], bg-[#fff], text-[14px]
 */

// ============================================================================
// Types
// ============================================================================

export type ClassObject = Record<string, unknown>;
export type ClassInput =
  | string
  | number
  | boolean
  | null
  | undefined
  | ClassObject
  | ClassInput[]
  | TokenRef;

export type TokenMap = (key: string) => string | null;

export interface ClassyOptions {
  /** Resolve token references to class strings */
  tokenMap?: TokenMap;
  /** Allow arbitrary values like w-[10px] (default: false) */
  allowArbitrary?: boolean;
  /** Custom warning handler */
  warn?: (msg: string) => void;
  /** Custom class normalization */
  normalize?: (cls: string) => string;
}

export interface TokenRef {
  __classy_token: string;
}

/** Result of parsing a Tailwind class */
export interface ParsedClass {
  /** Original class string */
  original: string;
  /** Modifier chain (e.g., ['hover', 'dark', 'data-[state=open]']) */
  modifiers: string[];
  /** The utility part (e.g., 'bg-blue-500', 'w-[100px]') */
  utility: string;
  /** Whether the utility contains an arbitrary value */
  isArbitrary: boolean;
  /** Whether this is a valid Tailwind class structure */
  isValid: boolean;
}

// ============================================================================
// Token Reference
// ============================================================================

export function token(key: string): TokenRef {
  return { __classy_token: key };
}

function isTokenRef(v: unknown): v is TokenRef {
  return !!v && typeof v === 'object' && typeof (v as TokenRef).__classy_token === 'string';
}

// ============================================================================
// Tailwind Class Parser
// ============================================================================

/**
 * Parse a Tailwind class into its components.
 * Handles bracket content correctly (brackets in modifiers vs arbitrary values).
 *
 * Examples:
 * - "hover:bg-blue-500" → modifiers: ['hover'], utility: 'bg-blue-500', isArbitrary: false
 * - "data-[state=open]:flex" → modifiers: ['data-[state=open]'], utility: 'flex', isArbitrary: false
 * - "w-[100px]" → modifiers: [], utility: 'w-[100px]', isArbitrary: true
 * - "hover:w-[100px]" → modifiers: ['hover'], utility: 'w-[100px]', isArbitrary: true
 * - "supports-[display:grid]:grid" → modifiers: ['supports-[display:grid]'], utility: 'grid', isArbitrary: false
 */
export function parseTailwindClass(className: string): ParsedClass {
  const original = className;
  const modifiers: string[] = [];

  // Find colon positions that are NOT inside brackets
  // These separate modifiers from each other and from the utility
  let depth = 0;
  let lastSplit = 0;
  const segments: string[] = [];

  for (let i = 0; i < className.length; i++) {
    const char = className[i];
    if (char === '[') {
      depth++;
    } else if (char === ']') {
      depth--;
    } else if (char === ':' && depth === 0) {
      segments.push(className.slice(lastSplit, i));
      lastSplit = i + 1;
    }
  }

  // Validate balanced brackets - if depth != 0, brackets are malformed
  if (depth !== 0) {
    return {
      original,
      modifiers: [],
      utility: className,
      isArbitrary: false,
      isValid: false,
    };
  }

  // Add the final segment (utility)
  segments.push(className.slice(lastSplit));

  // All segments except the last are modifiers
  for (let i = 0; i < segments.length - 1; i++) {
    const seg = segments[i];
    if (seg) modifiers.push(seg);
  }

  // The last segment is the utility
  const utility = segments[segments.length - 1] || '';

  // Check if utility contains brackets (arbitrary value)
  const isArbitrary = /\[.*\]/.test(utility);

  // Basic validity check
  const isValid = utility.length > 0;

  return {
    original,
    modifiers,
    utility,
    isArbitrary,
    isValid,
  };
}

/**
 * Check if a class contains an arbitrary value in the utility portion.
 * Brackets in modifiers (data-[state=open]:) are NOT arbitrary.
 * Brackets in the utility (w-[100px]) ARE arbitrary.
 */
export function hasArbitraryValue(className: string): boolean {
  return parseTailwindClass(className).isArbitrary;
}

// ============================================================================
// Utility Functions
// ============================================================================

function defaultWarn(msg: string) {
  if (typeof console !== 'undefined' && console?.warn) {
    console.warn(msg);
  }
}

function flatten(inputs: ClassInput[], out: unknown[] = []): unknown[] {
  for (const i of inputs) {
    if (i == null || i === false) continue;
    if (Array.isArray(i)) {
      flatten(i, out);
    } else {
      out.push(i);
    }
  }
  return out;
}

// ============================================================================
// Class Builder
// ============================================================================

export function createClassy(options?: ClassyOptions) {
  const tokenMap = options?.tokenMap;
  const allowArbitrary = options?.allowArbitrary ?? false;
  const warn = options?.warn ?? defaultWarn;
  const normalize = options?.normalize ?? ((s: string) => s);

  /**
   * Process a single class string, checking for arbitrary values
   */
  function processClass(cls: string, seen: Set<string>, out: string[]): void {
    if (!allowArbitrary && hasArbitraryValue(cls)) {
      warn(`classy: arbitrary value '${cls}' skipped`);
      return;
    }

    const norm = normalize(cls);
    if (!seen.has(norm)) {
      seen.add(norm);
      out.push(norm);
    }
  }

  /**
   * Process a space-separated class string
   */
  function processClassString(str: string, seen: Set<string>, out: string[]): void {
    for (const part of str.split(/\s+/)) {
      if (part) {
        processClass(part, seen, out);
      }
    }
  }

  /**
   * Build a class string from various inputs
   */
  function build(...inputs: ClassInput[]): string {
    const flat = flatten(inputs);
    const seen = new Set<string>();
    const out: string[] = [];

    for (const item of flat) {
      if (item == null || item === false) continue;

      // Token reference
      if (isTokenRef(item)) {
        const key = item.__classy_token;
        if (tokenMap) {
          const resolved = tokenMap(key);
          if (resolved) {
            processClassString(resolved, seen, out);
          } else {
            warn(`classy: unknown token '${key}'`);
          }
        } else {
          warn(`classy: token '${key}' used but no tokenMap provided`);
        }
        continue;
      }

      // Object form { 'class-name': boolean }
      if (typeof item === 'object') {
        for (const k of Object.keys(item as ClassObject)) {
          if ((item as ClassObject)[k]) {
            processClassString(k, seen, out);
          }
        }
        continue;
      }

      // Primitive (string/number/boolean)
      processClassString(String(item), seen, out);
    }

    return out.join(' ');
  }

  // Attach utilities to the build function
  const instance = Object.assign(build, {
    /** Create a token reference */
    token,
    /** Create a new classy instance with different options */
    create: createClassy,
    /** Parse a Tailwind class into components */
    parse: parseTailwindClass,
    /** Check if a class has arbitrary values */
    hasArbitrary: hasArbitraryValue,
  });

  return instance;
}

// ============================================================================
// Default Export
// ============================================================================

/** Default classy instance (arbitrary values blocked) */
export const classy = createClassy();

export default classy;
