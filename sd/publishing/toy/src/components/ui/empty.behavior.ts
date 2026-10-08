import type { BehaviorSpec } from '@/lib/contract';

/**
 * Empty: an empty-state placeholder. The composition archetype -- a static
 * score with NO state, NO actions, NO keymap, NO effects, and (like Card and
 * Container) an EMPTY, structural aria projection. The placeholder communicates
 * the absence of content through an icon, a heading, a description, and an
 * optional action; none of that carries a role the score must project, so the
 * projection is empty and the surface is pure decoration (a centered column of
 * muted, breathing space).
 *
 * Because the projection is empty and there is nothing to react to, Empty needs
 * NO client at all: there is no `bindEmpty`, the React performance uses no
 * `useBehavior`/`useMemory`, the Astro performance ships no `<script>`, and the
 * Web Component performs no binding. This is the same finding the card port
 * recorded -- a pure static's framework files are the thinnest possible: markup
 * + classes + slots, nothing more. The score is declared only so the
 * conformance harness can assert the one real contract (the `root` part renders
 * and projects no ARIA) identically across React, the Web Component, and Astro.
 *
 * The composition family (EmptyIcon, EmptyTitle, EmptyDescription, EmptyAction)
 * carries no behaviour of its own -- those are plain framework wrappers over
 * literal class strings, composed by the consumer inside an Empty. Only `Empty`
 * is a declared part, because it is the only node with a contract to project
 * (an empty one, but a declared part) -- boundary 5: a binding rendering an
 * undeclared part is structure the score never authorized. The heading
 * hierarchy the empty state relies on comes from `EmptyTitle` rendering a real
 * heading element, not from anything the score projects.
 */

export type EmptyConfig = Record<never, never>;
export type EmptyState = Record<never, never>;
export type EmptyActions = Record<never, never>;
export type EmptyPart = 'root';

export const empty: BehaviorSpec<EmptyConfig, EmptyState, EmptyActions, EmptyPart> = {
  name: 'empty',
  parts: { root: {} },
  initialState: () => ({}),
  actions: {},
  canDispatch: () => true,
  // The placeholder carries no role of its own; the heading inside supplies
  // structure. The score projects nothing and the harness asserts the empty
  // contract across every framework.
  aria: () => ({ root: {} }),
  keymap: () => null,
};
