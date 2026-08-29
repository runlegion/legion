/**
 * Inline toolbar primitive - position adjustment and format button configuration
 *
 * Provides viewport-aware positioning for a floating text formatting toolbar
 * and format button definitions with platform-aware shortcuts.
 *
 * @registry-name inline-toolbar
 * @registry-version 0.1.0
 * @registry-status published
 * @registry-path primitives/inline-toolbar.ts
 * @registry-type registry:primitive
 *
 * @cognitive-load 4/10 - Contextual formatting actions that appear on text selection
 * @attention-economics Appears on demand near selection, non-blocking; easy dismissal
 * @trust-building Predictable positioning (above selection, flips below if needed), clear active states, familiar shortcuts
 * @accessibility Full keyboard navigation through buttons, screen reader announcements, focus management
 * @semantic-meaning Format buttons map to semantic text marks (bold, italic, code, strikethrough, link)
 *
 * @usage-patterns
 * DO: Use adjustToolbarPosition() to keep toolbar within viewport bounds
 * DO: Use getFormatButtons() for consistent button config across frameworks
 * DO: Use isValidUrl/normalizeUrl for link popover validation
 * NEVER: Show on collapsed (zero-width) text selection
 * NEVER: Block editing while toolbar is visible
 *
 * @example
 * ```ts
 * const adjusted = adjustToolbarPosition(
 *   { x: 100, y: 50 },
 *   { width: 320, height: 44 },
 * );
 *
 * const buttons = getFormatButtons();
 * // [{ format: 'bold', label: 'Bold', shortcut: 'Cmd+B' }, ...]
 * ```
 */
import type { InlineMark } from "@/lib/primitives/types";

// ============================================================================
// Types
// ============================================================================

export interface ToolbarPosition {
  x: number;
  y: number;
}

export interface ToolbarDimensions {
  width: number;
  height: number;
}

export interface AdjustedToolbarPosition {
  x: number;
  y: number;
}

export interface FormatButtonConfig {
  format: InlineMark;
  label: string;
  shortcut: string;
}

// ============================================================================
// Platform detection
// ============================================================================

const isMac = typeof navigator !== "undefined" && /Mac|iPhone|iPad/.test(navigator.userAgent);
const modKey = isMac ? "Cmd" : "Ctrl";

// ============================================================================
// Format button configs
// ============================================================================

const FORMAT_BUTTONS: FormatButtonConfig[] = [
  { format: "bold", label: "Bold", shortcut: `${modKey}+B` },
  { format: "italic", label: "Italic", shortcut: `${modKey}+I` },
  { format: "code", label: "Code", shortcut: `${modKey}+E` },
  { format: "strikethrough", label: "Strikethrough", shortcut: `${modKey}+Shift+S` },
];

/**
 * Get the list of format button configurations for rendering.
 */
export function getFormatButtons(): FormatButtonConfig[] {
  return FORMAT_BUTTONS;
}

/**
 * Get the platform-aware modifier key label.
 */
export function getModifierKey(): string {
  return modKey;
}

// ============================================================================
// Positioning
// ============================================================================

/**
 * Adjust a toolbar position to stay within the viewport.
 * Prefers showing above the selection; flips below if not enough space.
 *
 * @param position - Desired anchor point (typically top-left of text selection)
 * @param dimensions - Toolbar dimensions (width, height)
 * @param selectionHeight - Approximate height of the text selection (default 24px)
 * @param viewportWidth - Override viewport width (for testing)
 * @param viewportHeight - Override viewport height (for testing)
 */
export function adjustToolbarPosition(
  position: ToolbarPosition,
  dimensions: ToolbarDimensions,
  selectionHeight = 24,
  viewportWidth?: number,
  viewportHeight?: number,
): AdjustedToolbarPosition {
  const vw = viewportWidth ?? (typeof window !== "undefined" ? window.innerWidth : 1024);
  const vh = viewportHeight ?? (typeof window !== "undefined" ? window.innerHeight : 768);
  const pad = 8;

  let x = position.x;
  let y: number;

  // Horizontal clamping
  if (x + dimensions.width > vw - pad) x = vw - dimensions.width - pad;
  if (x < pad) x = pad;

  // Vertical: prefer above, flip below if needed
  const aboveY = position.y - dimensions.height - pad;
  const belowY = position.y + selectionHeight;

  if (aboveY < pad) {
    y = belowY;
  } else if (belowY + dimensions.height > vh - pad) {
    y = aboveY;
  } else {
    y = aboveY;
  }

  return { x, y };
}

// ============================================================================
// URL validation (for link popover)
// ============================================================================

/**
 * Check if a string is a valid URL.
 * Accepts URLs without protocol (will be tested with https:// prefix).
 */
export function isValidUrl(urlString: string): boolean {
  if (!urlString.trim()) return false;
  const urlToTest = urlString.startsWith("http") ? urlString : `https://${urlString}`;
  try {
    new URL(urlToTest);
    return true;
  } catch {
    return false;
  }
}

/**
 * Normalize a URL by adding https:// if no protocol is present.
 */
export function normalizeUrl(urlString: string): string {
  if (urlString.startsWith("http://") || urlString.startsWith("https://")) {
    return urlString;
  }
  return `https://${urlString}`;
}
