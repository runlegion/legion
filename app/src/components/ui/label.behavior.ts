import type { BehaviorSpec } from "@/lib/contract";

/**
 * Label: a form control label. The composition archetype -- a static score
 * with NO state, NO actions, NO keymap, NO effects, and (like Card and
 * Container) an EMPTY, structural aria projection. A native `<label>` element
 * carries its own semantics, and the label-to-control association is the
 * native `for`/`htmlFor` IDREF attribute the consumer supplies -- so the score
 * projects nothing and owns no association logic. The surface is pure
 * decoration (a text-role token plus one of nine semantic colour variants).
 *
 * The label-control association is native, exactly as the oracle shipped it:
 * `<label for="id">` binds to `<control id="id">`. That mechanism is the
 * platform's, not the score's, so `for`/`htmlFor` is NEVER in config and
 * NEVER in the aria projection -- it passes through as a consumer-provided
 * native attribute (React `{...props}`, Astro `{...attrs}`, and the Web
 * Component forwards the host `for` onto the inner label). This is the
 * scroll-area parallel: "native scroll owns every semantic" becomes "native
 * `<label for>` owns the association." Preserving the mechanism exactly is why
 * the projection stays empty rather than reprojecting `for` through the score.
 *
 * Because the projection is empty and there is nothing to react to, Label
 * needs NO client at all: there is no `bindLabel`, the React performance uses
 * no `useBehavior`/`useMemory`, the Astro performance ships no `<script>`, and
 * the Web Component performs no binding. The score is declared only so the
 * conformance harness can assert the one real contract (the `root` part
 * renders and projects no ARIA) identically across React, the Web Component,
 * and Astro.
 */

export type LabelVariant =
  | "default"
  | "primary"
  | "secondary"
  | "destructive"
  | "success"
  | "warning"
  | "info"
  | "muted"
  | "accent";

export interface LabelConfig {
  /**
   * Semantic colour variant over the role vocabulary (rafters extension; the
   * shadcn base has no variant). Each maps to a `text-{role}` token, never a
   * raw colour utility. Defaults to `default` (the foreground token).
   */
  variant?: LabelVariant | undefined;
}

export type LabelState = Record<never, never>;
export type LabelActions = Record<never, never>;
export type LabelPart = "root";

export const label: BehaviorSpec<LabelConfig, LabelState, LabelActions, LabelPart> = {
  name: "label",
  parts: { root: {} },
  initialState: () => ({}),
  actions: {},
  canDispatch: () => true,
  // The `<label>` element carries its own semantics and the `for`/`htmlFor`
  // association is native to the consumer's markup; the score projects
  // nothing and the harness asserts the empty contract across every framework.
  aria: () => ({ root: {} }),
  keymap: () => null,
};
