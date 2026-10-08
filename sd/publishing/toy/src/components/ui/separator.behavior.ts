import type { BehaviorSpec } from '@/lib/contract';

/**
 * Separator: a visual divider. A static score -- no state, no actions, no
 * keymap, no effects -- but UNLIKE Card/Container/ScrollArea its aria
 * projection is NOT empty: a separator's role and orientation are the whole
 * accessible contract, and they are a pure function of config.
 *
 * Decorative by default (the shadcn/Radix default): a rule drawn purely for
 * visual grouping is `role="none"`, removed from the accessibility tree so no
 * assistive technology announces a meaningless boundary. Opt out
 * (`decorative={false}`) and the divider becomes a semantic `role="separator"`
 * carrying `aria-orientation`, announced as a section break.
 *
 * Because role/orientation depend only on config (never on state, never on an
 * interaction), the projection is computed once at render time and painted
 * directly by each performance -- there is NO `bindSeparator`, no
 * `useBehavior`/`useMemory`, no `<script>`, and the Web Component performs no
 * binding. The score is declared so the conformance harness can assert the one
 * real contract (the `root` part renders the projected role and
 * aria-orientation, or their absence) identically across React, the Web
 * Component, and Astro.
 */

export type SeparatorOrientation = 'horizontal' | 'vertical';

export interface SeparatorConfig {
  /** Rule axis. Default `horizontal`. */
  orientation?: SeparatorOrientation | undefined;
  /** Purely visual (removed from the a11y tree). Default `true`. */
  decorative?: boolean | undefined;
}

export type SeparatorState = Record<never, never>;
export type SeparatorActions = Record<never, never>;
export type SeparatorPart = 'root';

export const separator: BehaviorSpec<
  SeparatorConfig,
  SeparatorState,
  SeparatorActions,
  SeparatorPart
> = {
  name: 'separator',
  // role is projected, not declared on the part, because it VARIES with config
  // (none when decorative, separator when semantic) -- the part decl role is
  // for a fixed role, which a separator does not have.
  parts: { root: {} },
  initialState: () => ({}),
  actions: {},
  canDispatch: () => true,
  // The one real contract of a divider: decorative rules leave the a11y tree
  // (role="none", no orientation announced); semantic rules become a
  // role="separator" carrying aria-orientation.
  aria: (_state, config) => {
    const decorative = config.decorative ?? true;
    const orientation = config.orientation ?? 'horizontal';
    return {
      root: decorative ? { role: 'none' } : { role: 'separator', 'aria-orientation': orientation },
    };
  },
  keymap: () => null,
};
