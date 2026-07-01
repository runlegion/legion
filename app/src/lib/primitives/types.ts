/**
 * Shared primitive types
 */
export type CleanupFunction = () => void;

/**
 * Gamut tier: which CSS color space the color fits in.
 * - srgb: in sRGB (safe on every screen)
 * - p3:   in Display P3 but outside sRGB (wide-gamut displays only)
 * - out:  outside both gamuts (not displayable)
 */
export type GamutTier = 'srgb' | 'p3' | 'out';

export type OutsideClickHandler = (event: MouseEvent | TouchEvent | PointerEvent) => void;

export type EscapeKeyHandler = (event: KeyboardEvent) => void;

/**
 * Orientation for navigation primitives
 */
export type Orientation = 'horizontal' | 'vertical' | 'both';

/**
 * Text direction for RTL support
 */
export type Direction = 'ltr' | 'rtl';

/**
 * Keyboard event key names for type-safe handlers
 */
export type KeyboardKey =
  | 'Enter'
  | 'Space'
  | 'Escape'
  | 'Tab'
  | 'ArrowUp'
  | 'ArrowDown'
  | 'ArrowLeft'
  | 'ArrowRight'
  | 'Home'
  | 'End'
  | 'PageUp'
  | 'PageDown'
  | 'Backspace'
  | 'Delete';

/**
 * Modifier keys for keyboard handlers
 */
export interface KeyboardModifiers {
  shift?: boolean;
  ctrl?: boolean;
  alt?: boolean;
  meta?: boolean;
}

/**
 * Keyboard handler callback
 */
export type KeyboardHandlerCallback = (event: KeyboardEvent) => void;

/**
 * Live region politeness for screen reader announcements
 */
export type LiveRegionPoliteness = 'polite' | 'assertive' | 'off';

/**
 * Live region role for screen reader announcements
 */
export type LiveRegionRole = 'status' | 'alert' | 'log';

/**
 * Side positioning for floating elements
 */
export type Side = 'top' | 'right' | 'bottom' | 'left';

/**
 * Alignment for floating elements
 */
export type Align = 'start' | 'center' | 'end';

/**
 * Position result from collision detection
 */
export interface Position {
  x: number;
  y: number;
  side: Side;
  align: Align;
}

/**
 * Focus outside handler
 */
export type FocusOutsideHandler = (event: FocusEvent) => void;

/**
 * Pointer down outside handler
 */
export type PointerDownOutsideHandler = (event: PointerEvent | TouchEvent) => void;

/**
 * Navigation callback for roving focus
 */
export type NavigationCallback = (element: HTMLElement, index: number) => void;

/**
 * ARIA attribute types for type-safe attribute setting
 */
export interface AriaAttributes {
  // Widget attributes
  'aria-autocomplete'?: 'none' | 'inline' | 'list' | 'both';
  'aria-checked'?: boolean | 'mixed';
  'aria-disabled'?: boolean;
  'aria-expanded'?: boolean;
  'aria-haspopup'?: boolean | 'menu' | 'listbox' | 'tree' | 'grid' | 'dialog';
  'aria-hidden'?: boolean;
  'aria-invalid'?: boolean | 'grammar' | 'spelling';
  'aria-label'?: string;
  'aria-level'?: number;
  'aria-modal'?: boolean;
  'aria-multiline'?: boolean;
  'aria-multiselectable'?: boolean;
  'aria-orientation'?: 'horizontal' | 'vertical';
  'aria-placeholder'?: string;
  'aria-pressed'?: boolean | 'mixed';
  'aria-readonly'?: boolean;
  'aria-required'?: boolean;
  'aria-selected'?: boolean;
  'aria-sort'?: 'none' | 'ascending' | 'descending' | 'other';
  'aria-valuemax'?: number;
  'aria-valuemin'?: number;
  'aria-valuenow'?: number;
  'aria-valuetext'?: string;

  // Live region attributes
  'aria-atomic'?: boolean;
  'aria-busy'?: boolean;
  'aria-live'?: 'off' | 'polite' | 'assertive';
  'aria-relevant'?: 'additions' | 'removals' | 'text' | 'all' | 'additions text';

  // Relationship attributes
  'aria-activedescendant'?: string;
  'aria-controls'?: string;
  'aria-describedby'?: string;
  'aria-details'?: string;
  'aria-errormessage'?: string;
  'aria-flowto'?: string;
  'aria-labelledby'?: string;
  'aria-owns'?: string;
  'aria-posinset'?: number;
  'aria-setsize'?: number;
  'aria-colcount'?: number;
  'aria-colindex'?: number;
  'aria-colspan'?: number;
  'aria-rowcount'?: number;
  'aria-rowindex'?: number;
  'aria-rowspan'?: number;

  // Role attribute
  role?: string;
}

// =============================================================================
// Editor v1 Types
// =============================================================================

/**
 * Base block shape shared by the editor component and serialization primitives.
 * EditorBlock extends this with runtime-only fields (rules). Serializers work
 * with this shape so they stay decoupled from the React component layer.
 */
export interface BaseBlock {
  id: string;
  type: string;
  content?: string | InlineContent[];
  children?: string[];
  parentId?: string;
  meta?: Record<string, unknown>;
}

/**
 * Inline formatting mark types for rich text editing
 */
export type InlineMark = 'bold' | 'italic' | 'code' | 'strikethrough' | 'link';

/**
 * Rich text content with inline formatting marks
 */
export interface InlineContent {
  text: string;
  marks?: InlineMark[];
  /** Only present when marks includes 'link' */
  href?: string;
}

/**
 * Input event types for contenteditable handling
 */
export type InputType =
  | 'insertText'
  | 'insertParagraph'
  | 'insertLineBreak'
  | 'deleteContentBackward'
  | 'deleteContentForward'
  | 'deleteByCut'
  | 'insertFromPaste'
  | 'formatBold'
  | 'formatItalic'
  | 'formatUnderline'
  | 'formatStrikeThrough'
  | 'historyUndo'
  | 'historyRedo';

/**
 * Selection range for text selection primitive
 */
export interface SelectionRange {
  startNode: Node;
  startOffset: number;
  endNode: Node;
  endOffset: number;
  collapsed: boolean;
}

/**
 * Command definition for command palette
 */
export interface Command {
  id: string;
  label: string;
  description?: string;
  icon?: string;
  category?: string;
  keywords?: string[];
  shortcut?: string;
  action: () => void;
}

/**
 * Format definition for inline formatting
 */
export interface FormatDefinition {
  name: InlineMark;
  tag: string;
  shortcut?: string;
  attributes?: Record<string, string>;
  class?: string;
}

// =============================================================================
// Color Picker Types
// =============================================================================

/** Normalized 2D coordinate within a surface, both axes 0-1 */
export interface NormalizedPoint {
  /** Horizontal position, 0 = left edge, 1 = right edge */
  left: number;
  /** Vertical position, 0 = top edge, 1 = bottom edge */
  top: number;
}

/** Keyboard movement delta (additive offset applied to a NormalizedPoint) */
export interface MoveDelta {
  /** Horizontal delta, positive = rightward */
  dLeft: number;
  /** Vertical delta, positive = downward */
  dTop: number;
}

/**
 * Dimension mode for the interactive primitive.
 * '1d-horizontal' - only left axis is active (top locked to 0)
 * '1d-vertical' - only top axis is active (left locked to 0)
 * '2d' - both axes are active
 */
export type InteractiveMode = '1d-horizontal' | '1d-vertical' | '2d';

/** OKLCH color triplet without alpha */
export interface OklchColor {
  /** Lightness, 0 (black) to 1 (white) */
  l: number;
  /** Chroma, 0 (gray) to ~0.4 (most vivid in sRGB) -- no theoretical max */
  c: number;
  /** Hue angle in degrees, 0 to 360 (circular: 0 and 360 are equivalent) */
  h: number;
}

/** OKLCH color triplet with optional alpha */
export interface OklchColorAlpha extends OklchColor {
  /** Opacity, 0 (transparent) to 1 (opaque). Omit for fully opaque. */
  alpha?: number;
}
