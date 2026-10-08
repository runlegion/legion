import type { BehaviorSpec } from '@/lib/contract';

/**
 * Card: a content surface. The composition archetype -- a static score with
 * NO state, NO actions, NO keymap, NO effects, and (like Container) an
 * EMPTY, structural aria projection. A `div`/`section`/`article`/`aside`
 * carries its own native semantics, so the score projects nothing; the
 * surface is pure decoration (an elevated, bordered, rounded panel) plus an
 * optional `fill` signature over the colour vocabulary.
 *
 * Because the projection is empty and there is nothing to react to, Card
 * needs NO client at all: there is no `bindCard`, the React controller uses
 * no `useBehavior`/`useMemory`, the Astro performance ships no `<script>`,
 * and the Web Component performs no binding. This is the finding the card
 * port exists to record -- a pure static's framework files are the thinnest
 * possible: markup + classes + slots, nothing more. The score is declared
 * only so the conformance harness can assert the one real contract (the
 * `root` part renders and projects no ARIA) identically across React, the
 * Web Component, and Astro.
 *
 * The composition family (CardHeader, CardTitle, CardDescription,
 * CardContent, CardFooter, CardAction) carries no behaviour of its own --
 * those are plain framework wrappers over literal class strings, composed by
 * the consumer inside a Card. Only `Card` is a declared part, because it is
 * the only node with a contract to project (boundary 5: a binding rendering
 * an undeclared part is structure the score never authorized).
 */

export type CardElement = 'div' | 'article' | 'section' | 'aside';

export interface CardConfig {
  as?: CardElement | undefined;
  /**
   * Fill signature over the colour vocabulary (#1637): `word` (solid --
   * role words pair their foreground: fill="primary"), `word/alpha`
   * (Tailwind slash-opacity: fill="muted/50"), or `word-to-word` (two-stop
   * gradient: fill="primary-to-primary/0"). When it resolves, the fill
   * surface replaces the default `bg-card` surface; an invalid signature
   * resolves to nothing and the default surface stays. Fill, never a raw
   * background prop -- that legacy channel is dropped.
   */
  fill?: string | undefined;
}

export type CardState = Record<never, never>;
export type CardActions = Record<never, never>;
export type CardPart = 'root';

export const card: BehaviorSpec<CardConfig, CardState, CardActions, CardPart> = {
  name: 'card',
  parts: { root: {} },
  initialState: () => ({}),
  actions: {},
  canDispatch: () => true,
  // The surface is native to the element the config chooses; the score
  // projects nothing and the harness asserts the empty contract across
  // every framework.
  aria: () => ({ root: {} }),
  keymap: () => null,
};
