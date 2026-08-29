/**
 * Block-based document editor with op-based undo/redo history
 *
 * @cognitive-load 5/10 - Sustained composition attention; undo/redo gives a safety net
 * @attention-economics Full attention while composing -- the caret is the sole focus anchor
 * @trust-building Every edit is reversible (Cmd/Ctrl+Z, Cmd/Ctrl+Shift+Z), so mistakes cost nothing
 * @accessibility role=textbox, aria-multiline, a required accessible name (label or labelledBy)
 * @semantic-meaning Primary authoring surface: prose and structured blocks, not a form field
 *
 * @usage-patterns
 * DO: Always supply a real accessible name (label or labelledBy) -- axe fails an unnamed textbox
 * DO: Seed initial content via the decorator's own data, not a post-mount DOM write
 * NEVER: Mutate the contenteditable's DOM directly -- the model owns every edit
 *
 * @example
 * ```tsx
 * <Editor label="Document" />
 * ```
 */

/**
 * React performance for editor. Unlike most scores, this one is NOT fully
 * declarative: a contenteditable can't be safely virtual-DOM-diffed (Spec 00
 * -- the editor is own-project-scale, outside the composer, precisely because
 * of this), so `Editor` still delegates the actual binding to `bindEditor` --
 * the SAME DOM-native client the Web Component and Astro performances use --
 * and only adds the declarative layer `bindEditor` cannot: `useMemory` over
 * its own `createEditorHistory` cell, injected into `bindEditor` so both read
 * ONE cell, not two divergent ones.
 */
import * as React from "react";
import type { PartIds } from "@/lib/contract";
import { useMemory } from "@/hooks/use-memory";
import {
  bindEditor,
  editorAria,
  type EditorConfig,
  type EditorPart,
} from "@/components/ui/editor.behavior";
import { editorClasses } from "@/components/ui/editor.classes";
import { createEditorHistory } from "@/components/ui/editor-history";

export type EditorProps = (
  | { label: string; labelledBy?: undefined }
  | { label?: undefined; labelledBy: string }
) & {
  disabled?: boolean;
  readonly?: boolean;
};

export function Editor(props: EditorProps): React.JSX.Element {
  const { disabled, readonly } = props;
  const rootRef = React.useRef<HTMLDivElement | null>(null);
  const uid = React.useId();

  // Own createEditorHistory instance, injected into bindEditor below so the
  // DOM-native binder and useMemory here read the SAME cell -- bindEditor
  // would otherwise construct its own from root's data-* (the WC/Astro path).
  const history = React.useMemo(() => createEditorHistory(), []);
  const state = useMemory(history.memory);

  const config = props as EditorConfig;
  const ids = { root: uid } as PartIds<EditorPart>;
  const aria = editorAria(state, config, ids).root;
  const classes = editorClasses(config, state);

  React.useEffect(() => {
    const root = rootRef.current;
    if (!root) return;
    return bindEditor(root, history);
  }, [history]);

  return (
    <div
      ref={rootRef}
      id={uid}
      data-part="root"
      data-label={props.label}
      data-labelledby={props.labelledBy}
      data-disabled={disabled ? "true" : undefined}
      data-readonly={readonly ? "true" : undefined}
      className={classes.root}
      {...aria}
    />
  );
}
