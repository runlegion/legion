/**
 * editor.classes.ts -- the editor score's visual projection (FR-EDITOR-005).
 *
 * Root-only (settled): one class string, for the contenteditable root. No
 * per-block classes -- the contenteditable owns block DOM internally, and
 * this file makes no reducer/aria/keymap decision (those live in
 * editor.behavior.ts).
 */
import type { EditorConfig, EditorState } from "@/components/ui/editor.behavior";

export interface EditorClassSet {
  root: string;
}

// Disabled/readonly styling keys off data-disabled/data-readonly (the same
// data-* config attributes bindEditor itself reads via parseEditorConfig),
// NOT aria-disabled: editorAria never projects aria-disabled (the spec
// enumerates only role/aria-multiline/label), so an `aria-disabled:` variant
// here would be permanently dead. The exact-value arbitrary selector
// (`data-[disabled=true]:`, not the bare-presence `data-disabled:`) matters
// too: editor.astro always renders the attribute (`data-disabled={String(disabled)}`,
// so `data-disabled="false"` is present when NOT disabled) while editor.tsx
// omits it when false -- a presence-based selector would style the Astro
// performance as disabled even when it isn't.
const rootClasses =
  "min-h-32 w-full whitespace-pre-wrap rounded-md border border-input bg-transparent px-3 py-2 " +
  "text-body-medium ts-body-medium outline-none cursor-text " +
  "focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-2 " +
  "data-[disabled=true]:cursor-not-allowed data-[disabled=true]:opacity-50 " +
  "data-[readonly=true]:cursor-not-allowed data-[readonly=true]:opacity-50 " +
  "[&_a]:underline [&_code]:rounded [&_code]:bg-muted [&_code]:px-1 [&_code]:font-mono";

export function editorClasses(_config: EditorConfig, _state: EditorState): EditorClassSet {
  return {
    root: rootClasses,
  };
}
