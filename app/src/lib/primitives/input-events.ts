/**
 * Input events primitive for contenteditable handling
 * Manages beforeinput/input events with IME composition tracking
 * SSR-safe: checks for window existence
 */

import type { CleanupFunction, InputType } from "@/lib/primitives/types";

/**
 * Data extracted from an input event
 */
export interface InputData {
  inputType: InputType;
  data: string | null;
  isComposing: boolean;
  targetRanges: readonly StaticRange[];
}

/**
 * Options for creating an input handler
 */
export interface InputHandlerOptions {
  /** Element to attach handlers to */
  element: HTMLElement;
  /** Called after input event fires */
  onInput?: (data: InputData) => void;
  /** Called before input event fires, can be used to capture pre-change state */
  onBeforeInput?: (data: InputData) => void;
  /** Called when IME composition starts */
  onCompositionStart?: (event: CompositionEvent) => void;
  /** Called during IME composition */
  onCompositionUpdate?: (event: CompositionEvent) => void;
  /** Called when IME composition ends */
  onCompositionEnd?: (event: CompositionEvent) => void;
  /** Input types that should have their default behavior prevented */
  preventDefault?: InputType[];
}

/**
 * Return type for createInputHandler
 */
export interface InputHandler {
  /** Check if currently in IME composition mode */
  readonly isComposing: boolean;
  /** Cleanup function to remove all listeners */
  cleanup: CleanupFunction;
}

/**
 * Valid input types from the InputEvent specification
 * Used to validate and narrow InputEvent.inputType
 */
const VALID_INPUT_TYPES: ReadonlySet<InputType> = new Set([
  "insertText",
  "insertParagraph",
  "insertLineBreak",
  "deleteContentBackward",
  "deleteContentForward",
  "deleteByCut",
  "insertFromPaste",
  "formatBold",
  "formatItalic",
  "formatUnderline",
  "formatStrikeThrough",
  "historyUndo",
  "historyRedo",
]);

/**
 * Type guard to check if a string is a valid InputType
 */
function isValidInputType(inputType: string): inputType is InputType {
  return VALID_INPUT_TYPES.has(inputType as InputType);
}

/**
 * Create an input handler for contenteditable elements
 * Handles beforeinput/input events with IME composition tracking
 * Prevents duplicate characters during IME composition
 *
 * @example
 * const { isComposing, cleanup } = createInputHandler({
 *   element: editorElement,
 *   onInput: (data) => {
 *     if (data.inputType === 'insertText') {
 *       console.log('Text inserted:', data.data);
 *     }
 *   },
 *   onCompositionEnd: (event) => {
 *     console.log('IME composition finished:', event.data);
 *   },
 *   preventDefault: ['formatBold', 'formatItalic'],
 * });
 *
 * // Check composition state
 * if (!isComposing) {
 *   // Safe to process input
 * }
 *
 * // Cleanup when done
 * cleanup();
 */
export function createInputHandler(options: InputHandlerOptions): InputHandler {
  // SSR guard
  if (typeof window === "undefined") {
    return {
      get isComposing() {
        return false;
      },
      cleanup: () => {},
    };
  }

  const {
    element,
    onInput,
    onBeforeInput,
    onCompositionStart,
    onCompositionUpdate,
    onCompositionEnd,
    preventDefault = [],
  } = options;

  // Track composition state internally
  let composing = false;

  // Set of input types to prevent default on
  const preventDefaultSet = new Set<InputType>(preventDefault);

  /**
   * Extract InputData from an InputEvent
   */
  const extractInputData = (event: InputEvent): InputData | null => {
    const inputType = event.inputType;

    // Only handle known input types
    if (!isValidInputType(inputType)) {
      return null;
    }

    return {
      inputType,
      data: event.data,
      isComposing: event.isComposing || composing,
      // getTargetRanges() is only meaningful for contenteditable, and is absent
      // on the InputEvent of a native <input> in some engines (and in the test
      // environment). Calling it unguarded threw and took the whole listener
      // down, which meant this primitive could not be composed by a
      // native-input component at all. Absent ranges are simply none.
      targetRanges: typeof event.getTargetRanges === "function" ? event.getTargetRanges() : [],
    };
  };

  /**
   * Handle beforeinput event
   */
  const handleBeforeInput = (event: Event) => {
    // InputEvent check (beforeinput is an InputEvent)
    if (!(event instanceof InputEvent)) {
      return;
    }

    const data = extractInputData(event);
    if (!data) {
      return;
    }

    // Prevent default for configured input types
    if (preventDefaultSet.has(data.inputType)) {
      event.preventDefault();
    }

    // Skip processing during composition to prevent duplicate characters
    if (data.isComposing && data.inputType === "insertText") {
      return;
    }

    onBeforeInput?.(data);
  };

  /**
   * Handle input event
   */
  const handleInput = (event: Event) => {
    if (!(event instanceof InputEvent)) {
      return;
    }

    const data = extractInputData(event);
    if (!data) {
      return;
    }

    // Skip processing during composition to prevent duplicate characters
    // The final text will be handled in compositionend
    if (data.isComposing && data.inputType === "insertText") {
      return;
    }

    onInput?.(data);
  };

  /**
   * Handle compositionstart event
   */
  const handleCompositionStart = (event: CompositionEvent) => {
    composing = true;
    onCompositionStart?.(event);
  };

  /**
   * Handle compositionupdate event
   */
  const handleCompositionUpdate = (event: CompositionEvent) => {
    onCompositionUpdate?.(event);
  };

  /**
   * Handle compositionend event
   */
  const handleCompositionEnd = (event: CompositionEvent) => {
    composing = false;
    onCompositionEnd?.(event);
  };

  // Attach event listeners
  element.addEventListener("beforeinput", handleBeforeInput);
  element.addEventListener("input", handleInput);
  element.addEventListener("compositionstart", handleCompositionStart);
  element.addEventListener("compositionupdate", handleCompositionUpdate);
  element.addEventListener("compositionend", handleCompositionEnd);

  return {
    get isComposing() {
      return composing;
    },
    cleanup: () => {
      element.removeEventListener("beforeinput", handleBeforeInput);
      element.removeEventListener("input", handleInput);
      element.removeEventListener("compositionstart", handleCompositionStart);
      element.removeEventListener("compositionupdate", handleCompositionUpdate);
      element.removeEventListener("compositionend", handleCompositionEnd);
    },
  };
}
