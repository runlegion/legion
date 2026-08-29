/**
 * Design Intelligence Primitives
 * Pure behavioral functions for design reasoning - no styling concerns
 *
 * These primitives help components make informed decisions about:
 * - Cognitive load budgeting
 * - Accessibility validation
 * - Motion preferences
 */

/**
 * Calculate cognitive load for dialog configuration
 * Returns load score, budget, and recommendations for improvement
 *
 * Based on Rafters cognitive load budget: 15 points max per interaction
 */
export function calculateDialogCognitiveLoad(config: {
  hasTitle: boolean;
  hasDescription: boolean;
  hasForm: boolean;
  formFieldCount?: number;
  hasMultipleActions: boolean;
}): {
  load: number;
  budget: number;
  withinBudget: boolean;
  recommendations: string[];
} {
  let load = 0;
  const recommendations: string[] = [];

  // Base dialog: 2 points
  load += 2;

  // Title: 1 point
  if (config.hasTitle) load += 1;

  // Description: 1 point
  if (config.hasDescription) load += 1;

  // Form: 3 points base + 1 per field
  if (config.hasForm) {
    load += 3;
    load += (config.formFieldCount || 0) * 1;

    if (config.formFieldCount && config.formFieldCount > 5) {
      recommendations.push("Consider breaking form into multiple steps (wizard pattern)");
    }
  }

  // Multiple actions: 2 points
  if (config.hasMultipleActions) {
    load += 2;
  }

  const budget = 15;
  const withinBudget = load <= budget;

  if (!withinBudget) {
    recommendations.push(`Cognitive load (${load}) exceeds budget (${budget})`);
    recommendations.push("Simplify dialog content or break into multiple steps");
  }

  return { load, budget, withinBudget, recommendations };
}

/**
 * Validate dialog accessibility against WCAG requirements
 * Returns validation result with errors and warnings
 */
export function validateDialogAccessibility(config: {
  hasTitle: boolean;
  hasDescription: boolean;
  hasFocusableElements: boolean;
  modal: boolean;
}): {
  valid: boolean;
  errors: string[];
  warnings: string[];
} {
  const errors: string[] = [];
  const warnings: string[] = [];

  // WCAG requirement: dialogs must have accessible name
  if (!config.hasTitle) {
    errors.push("Dialog must have a title (aria-labelledby) for WCAG 2.1 compliance");
  }

  // Best practice: dialogs should have description
  if (!config.hasDescription) {
    warnings.push("Consider adding a description (aria-describedby) for better UX");
  }

  // Modal dialogs must trap focus
  if (config.modal && !config.hasFocusableElements) {
    errors.push("Modal dialogs must contain at least one focusable element");
  }

  return {
    valid: errors.length === 0,
    errors,
    warnings,
  };
}

/*
 * `getAnimationTiming(preference)` lived here and is retired (#1995).
 *
 * It returned a hardcoded 200ms and `cubic-bezier(0.16, 1, 0.3, 1)` -- an
 * invented seventh curve, in the primitives layer, matching none of the six the
 * system ships. A grep across `packages/` found no callers: only the definition
 * and its two listings in the primitives spec matrix, both removed with it.
 *
 * Its one correct behaviour -- zero duration under prefers-reduced-motion --
 * moves to `motion-tokens.ts`, where it applies to the `duration` and `delay`
 * namespaces and exempts `period`, because a stopped work loop says the work
 * stopped. Callers that want a timing ask the accessor for a named token.
 */

/**
 * Detect user's motion preference from media query
 * SSR-safe: returns 'normal' on server
 */
export function detectMotionPreference(): "reduced" | "normal" {
  if (typeof window === "undefined") {
    return "normal";
  }

  return window.matchMedia("(prefers-reduced-motion: reduce)").matches ? "reduced" : "normal";
}

/**
 * Calculate cognitive load for a generic component configuration
 * Extensible for different component types
 */
export function calculateComponentCognitiveLoad(config: {
  componentType: "dialog" | "form" | "menu" | "tooltip";
  elementCount: number;
  hasAnimation: boolean;
  interactionComplexity: "low" | "medium" | "high";
}): {
  load: number;
  budget: number;
  withinBudget: boolean;
} {
  const baseCosts: Record<string, number> = {
    dialog: 2,
    form: 3,
    menu: 2,
    tooltip: 1,
  };

  const complexityCosts: Record<string, number> = {
    low: 0,
    medium: 2,
    high: 4,
  };

  let load = baseCosts[config.componentType] || 1;
  load += config.elementCount * 0.5;
  load += config.hasAnimation ? 1 : 0;
  load += complexityCosts[config.interactionComplexity] || 0;

  const budget = 15;

  return {
    load: Math.round(load * 10) / 10,
    budget,
    withinBudget: load <= budget,
  };
}
