/**
 * ARIA Manager primitive
 * Type-safe ARIA attribute setting with validation
 *
 * WCAG Compliance:
 * - 4.1.2 Name, Role, Value (Level A): Correct ARIA usage
 * - Follows ARIA 1.2 specification for valid attribute values
 *
 * @example
 * ```typescript
 * const cleanup = setAriaAttributes(element, {
 *   'aria-expanded': true,
 *   'aria-controls': 'menu-1',
 *   role: 'button',
 * });
 * ```
 */

import type { AriaAttributes, CleanupFunction } from '@/lib/primitives/types';

export interface AriaManagerOptions {
  /**
   * Whether to validate attribute values against ARIA spec
   * @default true
   */
  validate?: boolean;

  /**
   * Whether to log warnings for invalid values
   * @default true (in development)
   */
  warn?: boolean;
}

/**
 * ARIA attribute value types for validation
 */
type AriaValueType =
  | 'boolean'
  | 'tristate'
  | 'string'
  | 'number'
  | 'token'
  | 'tokenlist'
  | 'idref'
  | 'idreflist';

/**
 * ARIA attribute definitions for validation
 */
const ARIA_DEFINITIONS: Record<string, { type: AriaValueType; values?: readonly string[] }> = {
  // Widget attributes
  'aria-autocomplete': { type: 'token', values: ['none', 'inline', 'list', 'both'] },
  'aria-checked': { type: 'tristate' },
  'aria-disabled': { type: 'boolean' },
  'aria-expanded': { type: 'boolean' },
  'aria-haspopup': {
    type: 'token',
    values: ['true', 'false', 'menu', 'listbox', 'tree', 'grid', 'dialog'],
  },
  'aria-hidden': { type: 'boolean' },
  'aria-invalid': { type: 'token', values: ['true', 'false', 'grammar', 'spelling'] },
  'aria-label': { type: 'string' },
  'aria-level': { type: 'number' },
  'aria-modal': { type: 'boolean' },
  'aria-multiline': { type: 'boolean' },
  'aria-multiselectable': { type: 'boolean' },
  'aria-orientation': { type: 'token', values: ['horizontal', 'vertical'] },
  'aria-placeholder': { type: 'string' },
  'aria-pressed': { type: 'tristate' },
  'aria-readonly': { type: 'boolean' },
  'aria-required': { type: 'boolean' },
  'aria-selected': { type: 'boolean' },
  'aria-sort': { type: 'token', values: ['none', 'ascending', 'descending', 'other'] },
  'aria-valuemax': { type: 'number' },
  'aria-valuemin': { type: 'number' },
  'aria-valuenow': { type: 'number' },
  'aria-valuetext': { type: 'string' },

  // Live region attributes
  'aria-atomic': { type: 'boolean' },
  'aria-busy': { type: 'boolean' },
  'aria-live': { type: 'token', values: ['off', 'polite', 'assertive'] },
  'aria-relevant': { type: 'tokenlist', values: ['additions', 'removals', 'text', 'all'] },

  // Relationship attributes
  'aria-activedescendant': { type: 'idref' },
  'aria-controls': { type: 'idreflist' },
  'aria-describedby': { type: 'idreflist' },
  'aria-details': { type: 'idref' },
  'aria-errormessage': { type: 'idref' },
  'aria-flowto': { type: 'idreflist' },
  'aria-labelledby': { type: 'idreflist' },
  'aria-owns': { type: 'idreflist' },
  'aria-posinset': { type: 'number' },
  'aria-setsize': { type: 'number' },
  'aria-colcount': { type: 'number' },
  'aria-colindex': { type: 'number' },
  'aria-colspan': { type: 'number' },
  'aria-rowcount': { type: 'number' },
  'aria-rowindex': { type: 'number' },
  'aria-rowspan': { type: 'number' },

  // Role attribute (not technically ARIA, but commonly grouped)
  role: { type: 'token' },
};

/**
 * Validate an ARIA attribute value
 */
function validateAriaValue(
  attribute: string,
  value: unknown,
  warn: boolean,
): { valid: boolean; stringValue: string } {
  const definition = ARIA_DEFINITIONS[attribute];

  if (!definition) {
    // Unknown attribute - might be valid ARIA we don't know about
    if (warn && attribute.startsWith('aria-')) {
      console.warn(`Unknown ARIA attribute: ${attribute}`);
    }
    return { valid: true, stringValue: String(value) };
  }

  let stringValue: string;
  let valid = true;

  switch (definition.type) {
    case 'boolean':
      if (typeof value !== 'boolean') {
        if (warn) console.warn(`ARIA attribute ${attribute} expects boolean, got ${typeof value}`);
        valid = false;
      }
      stringValue = value ? 'true' : 'false';
      break;

    case 'tristate':
      if (typeof value === 'boolean') {
        stringValue = value ? 'true' : 'false';
      } else if (value === 'mixed') {
        stringValue = 'mixed';
      } else {
        if (warn)
          console.warn(`ARIA attribute ${attribute} expects boolean or 'mixed', got ${value}`);
        valid = false;
        stringValue = String(value);
      }
      break;

    case 'number':
      if (typeof value !== 'number' || !Number.isFinite(value)) {
        if (warn) console.warn(`ARIA attribute ${attribute} expects number, got ${typeof value}`);
        valid = false;
      }
      stringValue = String(value);
      break;

    case 'token':
      stringValue = String(value);
      // Handle boolean-like tokens (true/false -> 'true'/'false')
      if (typeof value === 'boolean') {
        stringValue = value ? 'true' : 'false';
      }
      if (definition.values && !definition.values.includes(stringValue)) {
        if (warn) {
          console.warn(
            `ARIA attribute ${attribute} expects one of [${definition.values.join(', ')}], got ${stringValue}`,
          );
        }
        valid = false;
      }
      break;

    case 'tokenlist':
      if (Array.isArray(value)) {
        stringValue = value.join(' ');
      } else {
        stringValue = String(value);
      }
      break;

    case 'idref':
    case 'idreflist':
      if (Array.isArray(value)) {
        stringValue = value.join(' ');
      } else {
        stringValue = String(value);
      }
      break;

    default:
      stringValue = String(value);
      break;
  }

  return { valid, stringValue };
}

/**
 * Store for original attribute values
 */
type OriginalAttributes = Map<string, string | null>;

/**
 * Set multiple ARIA attributes on an element
 * Returns cleanup function that restores original values
 *
 * @example
 * ```typescript
 * const cleanup = setAriaAttributes(button, {
 *   'aria-expanded': true,
 *   'aria-controls': 'menu-1',
 *   'aria-haspopup': 'menu',
 * });
 *
 * // Later, restore original values
 * cleanup();
 * ```
 */
export function setAriaAttributes(
  element: HTMLElement,
  attributes: Partial<AriaAttributes>,
  options: AriaManagerOptions = {},
): CleanupFunction {
  // SSR guard
  if (typeof window === 'undefined') {
    return () => {};
  }

  const { validate = true, warn = true } = options;

  const originalAttributes: OriginalAttributes = new Map();

  for (const [key, value] of Object.entries(attributes)) {
    if (value === undefined) continue;

    // Store original value
    originalAttributes.set(key, element.getAttribute(key));

    // Validate and convert value
    if (validate) {
      const { stringValue } = validateAriaValue(key, value, warn);
      element.setAttribute(key, stringValue);
    } else {
      // Convert value to string directly
      let stringValue: string;
      if (typeof value === 'boolean') {
        stringValue = value ? 'true' : 'false';
      } else {
        stringValue = String(value);
      }
      element.setAttribute(key, stringValue);
    }
  }

  return () => {
    for (const [key, originalValue] of originalAttributes) {
      if (originalValue === null) {
        element.removeAttribute(key);
      } else {
        element.setAttribute(key, originalValue);
      }
    }
  };
}

/**
 * Update a single ARIA attribute
 *
 * @example
 * ```typescript
 * updateAriaAttribute(button, 'aria-expanded', true);
 * ```
 */
export function updateAriaAttribute<K extends keyof AriaAttributes>(
  element: HTMLElement,
  attribute: K,
  value: AriaAttributes[K],
  options: AriaManagerOptions = {},
): void {
  if (typeof window === 'undefined') return;

  const { validate = true, warn = true } = options;

  if (value === undefined) {
    element.removeAttribute(attribute);
    return;
  }

  if (validate) {
    const { stringValue } = validateAriaValue(attribute, value, warn);
    element.setAttribute(attribute, stringValue);
  } else {
    let stringValue: string;
    if (typeof value === 'boolean') {
      stringValue = value ? 'true' : 'false';
    } else {
      stringValue = String(value);
    }
    element.setAttribute(attribute, stringValue);
  }
}

/**
 * Remove multiple ARIA attributes
 *
 * @example
 * ```typescript
 * removeAriaAttributes(element, ['aria-expanded', 'aria-controls']);
 * ```
 */
export function removeAriaAttributes(
  element: HTMLElement,
  attributes: (keyof AriaAttributes)[],
): void {
  if (typeof window === 'undefined') return;

  for (const attr of attributes) {
    element.removeAttribute(attr);
  }
}

/**
 * Get an ARIA attribute value with proper type conversion
 *
 * @example
 * ```typescript
 * const expanded = getAriaAttribute(button, 'aria-expanded');
 * // Returns: true | false | undefined
 * ```
 */
export function getAriaAttribute<K extends keyof AriaAttributes>(
  element: HTMLElement,
  attribute: K,
): AriaAttributes[K] | undefined {
  if (typeof window === 'undefined') return undefined;

  const value = element.getAttribute(attribute);
  if (value === null) return undefined;

  const definition = ARIA_DEFINITIONS[attribute];
  if (!definition) return value as AriaAttributes[K];

  switch (definition.type) {
    case 'boolean':
      return (value === 'true') as AriaAttributes[K];

    case 'tristate':
      if (value === 'true') return true as AriaAttributes[K];
      if (value === 'false') return false as AriaAttributes[K];
      return value as AriaAttributes[K]; // 'mixed'

    case 'number': {
      const num = parseFloat(value);
      return (Number.isFinite(num) ? num : undefined) as AriaAttributes[K];
    }

    default:
      return value as AriaAttributes[K];
  }
}

/**
 * Check if an element has a specific ARIA attribute
 */
export function hasAriaAttribute(element: HTMLElement, attribute: keyof AriaAttributes): boolean {
  if (typeof window === 'undefined') return false;
  return element.hasAttribute(attribute);
}

/**
 * Toggle a boolean ARIA attribute
 *
 * @example
 * ```typescript
 * toggleAriaAttribute(button, 'aria-expanded');
 * // If was true, now false. If was false or missing, now true.
 * ```
 */
export function toggleAriaAttribute(
  element: HTMLElement,
  attribute: keyof AriaAttributes,
  force?: boolean,
): boolean {
  if (typeof window === 'undefined') return false;

  const current = element.getAttribute(attribute) === 'true';
  const newValue = force !== undefined ? force : !current;

  element.setAttribute(attribute, newValue ? 'true' : 'false');
  return newValue;
}

/**
 * Create ARIA relationship between elements
 * Automatically generates IDs if needed
 *
 * @example
 * ```typescript
 * const cleanup = createAriaRelationship(input, label, 'aria-labelledby');
 * ```
 */
export function createAriaRelationship(
  element: HTMLElement,
  target: HTMLElement | HTMLElement[],
  relationship: 'aria-labelledby' | 'aria-describedby' | 'aria-controls' | 'aria-owns',
): CleanupFunction {
  if (typeof window === 'undefined') {
    return () => {};
  }

  const targets = Array.isArray(target) ? target : [target];
  const generatedIds: string[] = [];

  // Ensure all targets have IDs
  const ids = targets.map((t) => {
    if (!t.id) {
      const id = `aria-${Date.now()}-${Math.random().toString(36).slice(2, 9)}`;
      t.id = id;
      generatedIds.push(id);
    }
    return t.id;
  });

  // Store original value
  const originalValue = element.getAttribute(relationship);

  // Set relationship
  const existingIds = originalValue ? originalValue.split(/\s+/) : [];
  const allIds = [...new Set([...existingIds, ...ids])];
  element.setAttribute(relationship, allIds.join(' '));

  return () => {
    // Restore original value
    if (originalValue === null) {
      element.removeAttribute(relationship);
    } else {
      element.setAttribute(relationship, originalValue);
    }

    // Remove generated IDs
    for (const id of generatedIds) {
      const el = document.getElementById(id);
      if (el) el.removeAttribute('id');
    }
  };
}
