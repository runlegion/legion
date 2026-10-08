import type { Slice } from '@/lib/compose';
import type { PartDecl } from '@/lib/contract';

/**
 * The disclosable slice: something with a trigger that reveals content.
 * The open/closed axis for dialog, popover, sheet, accordion item.
 *
 * Controlled/uncontrolled per the ownership-of-truth boundary: config.open
 * is the consumer's controlled value, passed fresh and never stored;
 * state.open is the intrinsic value the component originates. Projections
 * and gates read the EFFECTIVE value via isOpen().
 */
export interface DisclosableConfig {
  open?: boolean | undefined;
  defaultOpen?: boolean | undefined;
}

export interface DisclosableState {
  open: boolean;
}

export type DisclosableActions = {
  open: undefined;
  close: undefined;
};

export type DisclosablePart = 'trigger' | 'content';

export function isOpen(state: DisclosableState, config: DisclosableConfig): boolean {
  return config.open ?? state.open;
}

const parts: Record<DisclosablePart, PartDecl> = {
  trigger: {},
  content: { optional: true },
};

export function disclosable<Config extends DisclosableConfig>(): Slice<
  Config,
  DisclosableState,
  DisclosableActions,
  DisclosablePart
> {
  return {
    name: 'disclosable',
    parts,
    initialState: (config) => ({
      open: config.open ?? config.defaultOpen ?? false,
    }),
    actions: {
      open: (state) => ({ ...state, open: true }),
      close: (state) => ({ ...state, open: false }),
    },
    // Idempotence gate: opening the open or closing the closed is rejected,
    // so consumer callbacks fire once per real transition.
    canDispatch: (state, action, config) => {
      if (action === 'open') return !isOpen(state, config);
      if (action === 'close') return isOpen(state, config);
      return true;
    },
    aria: (state, config, ids) => {
      const open = isOpen(state, config);
      return {
        trigger: {
          'aria-expanded': open ? 'true' : 'false',
          // Empty-id sentinel (Spec 01, ruled 2026-07-08): reference the
          // content only when its id is real. Keying on `open` alone leaks
          // aria-controls="" on the first paint of an initially-open
          // disclosure, before the content ref has registered.
          'aria-controls': open && ids.content ? ids.content : undefined,
          'data-state': open ? 'open' : 'closed',
        },
        content: {
          'data-state': open ? 'open' : 'closed',
        },
      };
    },
    keymap: () => null,
  };
}
