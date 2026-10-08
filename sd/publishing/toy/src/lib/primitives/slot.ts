/**
 * Slot primitive for prop merging (asChild pattern)
 * Enables component composition by merging props from parent to child
 *
 * WCAG Compliance:
 * - 4.1.2 Name, Role, Value (Level A): Preserves ARIA attributes during merge
 *
 * @example
 * ```typescript
 * // Merge parent button props onto child anchor
 * const cleanup = mergeSlotProps(buttonElement, anchorElement, {
 *   mergeEventHandlers: true,
 *   mergeClassName: true,
 * });
 * ```
 */

import type * as React from 'react';
import classy from '@/lib/primitives/classy';
import type { CleanupFunction } from '@/lib/primitives/types';

export interface SlotMergeOptions {
  /**
   * Whether to merge ARIA attributes
   * @default true
   */
  mergeAria?: boolean;

  /**
   * Whether to merge data attributes
   * @default true
   */
  mergeData?: boolean;

  /**
   * Whether to merge className/class attributes
   * @default true
   */
  mergeClassName?: boolean;

  /**
   * Whether to merge inline styles
   * @default true
   */
  mergeStyle?: boolean;

  /**
   * Whether to merge event handlers
   * Event handlers are composed: child runs first, then parent
   * @default true
   */
  mergeEventHandlers?: boolean;

  /**
   * Custom class merger function
   * If not provided, classes are simply concatenated
   */
  classMerger?: (parentClass: string, childClass: string) => string;
}

/**
 * Standard event handler attribute names
 */
const EVENT_HANDLER_NAMES = [
  'onclick',
  'onkeydown',
  'onkeyup',
  'onkeypress',
  'onfocus',
  'onblur',
  'onmousedown',
  'onmouseup',
  'onmouseover',
  'onmouseout',
  'onmouseenter',
  'onmouseleave',
  'onpointerdown',
  'onpointerup',
  'ontouchstart',
  'ontouchend',
] as const;

/**
 * Get all ARIA attributes from an element
 */
function getAriaAttributes(element: Element): Map<string, string> {
  const attrs = new Map<string, string>();
  for (const attr of element.attributes) {
    if (attr.name.startsWith('aria-') || attr.name === 'role') {
      attrs.set(attr.name, attr.value);
    }
  }
  return attrs;
}

/**
 * Get all data attributes from an element
 */
function getDataAttributes(element: Element): Map<string, string> {
  const attrs = new Map<string, string>();
  for (const attr of element.attributes) {
    if (attr.name.startsWith('data-')) {
      attrs.set(attr.name, attr.value);
    }
  }
  return attrs;
}

/**
 * Store for original attribute values to restore on cleanup
 */
interface OriginalState {
  attributes: Map<string, string | null>;
  className: string;
  style: string;
  eventHandlers: Map<string, EventListener>;
}

/**
 * Merge props from parent element onto child element
 * Returns cleanup function that restores original child state
 *
 * This enables the "asChild" pattern where a wrapper component
 * can pass its props to a child element, allowing the child
 * to take on the wrapper's behavior while maintaining its own semantics.
 *
 * @example
 * ```typescript
 * // Parent Button wants to render as child Anchor
 * const cleanup = mergeSlotProps(buttonWrapper, anchorChild, {
 *   mergeClassName: true,
 *   mergeEventHandlers: true,
 * });
 *
 * // Later restore original state
 * cleanup();
 * ```
 */
export function mergeSlotProps(
  parent: HTMLElement,
  child: HTMLElement,
  options: SlotMergeOptions = {},
): CleanupFunction {
  // SSR guard
  if (typeof window === 'undefined') {
    return () => {};
  }

  const {
    mergeAria = true,
    mergeData = true,
    mergeClassName = true,
    mergeStyle = true,
    mergeEventHandlers = true,
    classMerger,
  } = options;

  // Store original state for cleanup
  const originalState: OriginalState = {
    attributes: new Map(),
    className: child.className,
    style: child.getAttribute('style') || '',
    eventHandlers: new Map(),
  };

  // Cleanup handlers to remove
  const cleanupHandlers: Array<() => void> = [];

  // Merge ARIA attributes
  if (mergeAria) {
    const parentAria = getAriaAttributes(parent);
    for (const [name, value] of parentAria) {
      // Store original value (or null if not present)
      originalState.attributes.set(name, child.getAttribute(name));
      child.setAttribute(name, value);
    }
  }

  // Merge data attributes
  if (mergeData) {
    const parentData = getDataAttributes(parent);
    for (const [name, value] of parentData) {
      originalState.attributes.set(name, child.getAttribute(name));
      child.setAttribute(name, value);
    }
  }

  // Merge className
  if (mergeClassName && parent.className) {
    const parentClass = parent.className;
    const childClass = child.className;

    if (classMerger) {
      child.className = classMerger(parentClass, childClass);
    } else {
      // Simple concatenation, preserving both
      child.className = childClass ? `${childClass} ${parentClass}` : parentClass;
    }
  }

  // Merge inline styles
  if (mergeStyle) {
    const parentStyle = parent.getAttribute('style');
    if (parentStyle) {
      const childStyle = child.getAttribute('style') || '';
      // Child styles take precedence (come after parent)
      child.setAttribute('style', `${parentStyle}; ${childStyle}`.replace(/^;\s*/, ''));
    }
  }

  // Merge event handlers
  if (mergeEventHandlers) {
    for (const handlerName of EVENT_HANDLER_NAMES) {
      // Check if parent has an inline handler or listener
      const parentHandler = (parent as unknown as Record<string, unknown>)[handlerName];
      if (typeof parentHandler === 'function') {
        const eventType = handlerName.slice(2); // Remove 'on' prefix
        const childHandler = (child as unknown as Record<string, unknown>)[handlerName];

        // Compose handlers: child first, then parent
        const composedHandler = (event: Event) => {
          if (typeof childHandler === 'function') {
            childHandler.call(child, event);
          }
          if (!event.defaultPrevented) {
            (parentHandler as EventListener).call(parent, event);
          }
        };

        child.addEventListener(eventType, composedHandler);

        cleanupHandlers.push(() => {
          child.removeEventListener(eventType, composedHandler);
        });
      }
    }
  }

  // Return cleanup function
  return () => {
    // Restore original attributes
    for (const [name, originalValue] of originalState.attributes) {
      if (originalValue === null) {
        child.removeAttribute(name);
      } else {
        child.setAttribute(name, originalValue);
      }
    }

    // Restore original className
    if (mergeClassName) {
      child.className = originalState.className;
    }

    // Restore original style
    if (mergeStyle) {
      if (originalState.style) {
        child.setAttribute('style', originalState.style);
      } else {
        child.removeAttribute('style');
      }
    }

    // Remove composed event handlers
    for (const cleanup of cleanupHandlers) {
      cleanup();
    }
  };
}

/**
 * Extract props from an element as a plain object
 * Useful for React/framework integration
 */
export function extractSlotProps(element: Element): Record<string, string> {
  if (typeof window === 'undefined') {
    return {};
  }

  const props: Record<string, string> = {};

  for (const attr of element.attributes) {
    props[attr.name] = attr.value;
  }

  return props;
}

/**
 * Check if an element should use slot composition
 * Utility for framework integration
 */
export function shouldUseSlot(asChild?: boolean): boolean {
  return asChild === true;
}

/**
 * Merge two className strings with deduplication
 * Simple utility that can be used as classMerger option
 */
export function mergeClassNames(parentClass: string, childClass: string): string {
  const parentClasses = parentClass.split(/\s+/).filter(Boolean);
  const childClasses = childClass.split(/\s+/).filter(Boolean);

  // Child classes take precedence, so they come last
  const seen = new Set<string>();
  const result: string[] = [];

  // Add parent classes first
  for (const cls of parentClasses) {
    if (!seen.has(cls)) {
      seen.add(cls);
      result.push(cls);
    }
  }

  // Add child classes (may override parent)
  for (const cls of childClasses) {
    if (!seen.has(cls)) {
      seen.add(cls);
      result.push(cls);
    }
  }

  return result.join(' ');
}

/**
 * Props object for framework-agnostic prop merging
 */
export interface SlotProps {
  className?: string | undefined;
  style?: React.CSSProperties | undefined;
  [key: string]: unknown;
}

/**
 * Merge two prop objects (for React/framework use)
 * Handles className, style, and event handlers specially
 */
export function mergeProps(
  parentProps: SlotProps,
  childProps: SlotProps,
  options: { classMerger?: (a: string, b: string) => string } = {},
): SlotProps {
  const { classMerger } = options;
  const merged: SlotProps = { ...parentProps };

  for (const key of Object.keys(childProps)) {
    const parentValue = parentProps[key];
    const childValue = childProps[key];

    // Handle className specially - use classy for deduplication and proper merging
    if (key === 'className') {
      merged.className = classMerger
        ? classMerger(String(parentValue ?? ''), String(childValue ?? ''))
        : classy(parentValue as string, childValue as string);
      continue;
    }

    // Handle style specially (merge objects)
    if (key === 'style' && typeof parentValue === 'object' && typeof childValue === 'object') {
      merged.style = {
        ...(parentValue as Record<string, string>),
        ...(childValue as Record<string, string>),
      };
      continue;
    }

    // Handle event handlers specially (compose functions)
    if (
      key.startsWith('on') &&
      typeof parentValue === 'function' &&
      typeof childValue === 'function'
    ) {
      merged[key] = (...args: unknown[]) => {
        (childValue as (...args: unknown[]) => void)(...args);
        (parentValue as (...args: unknown[]) => void)(...args);
      };
      continue;
    }

    // Default: child value overrides
    merged[key] = childValue;
  }

  return merged;
}
