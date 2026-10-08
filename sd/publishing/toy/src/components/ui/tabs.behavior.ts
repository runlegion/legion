import { compose, type Slice } from '@/lib/compose';
import {
  createBehavior,
  type AriaAttrs,
  type BehaviorSpec,
  type InstanceIds,
  type PartIds,
} from '@/lib/contract';
import { updateAriaAttribute } from '@/lib/primitives/aria-manager';
import { createRovingFocus } from '@/lib/primitives/roving-focus';

/**
 * Tabs: a list of triggers, exactly one active, each disclosing its panel.
 * Ports the old imperative tabs controller wholesale.
 *
 * The score's only state axis is which tab is active. Focus movement across
 * triggers is NOT state -- it is ephemeral DOM state owned by the roving-focus
 * primitive (mirroring radio-group/navigation-menu). Tabs use AUTOMATIC
 * activation: roving moves focus and the newly focused tab becomes active in
 * the same gesture. That is the oracle controller's behavior and the WAI-ARIA
 * APG default for tab sets whose panels are already in the DOM.
 *
 * Controlled/uncontrolled per the ownership-of-truth boundary applied to a
 * string, the same shape as radio-group/navigation-menu: `config.value` is the
 * consumer's controlled value (passed fresh, never stored); `state.value` is the
 * intrinsic value seeded from `defaultValue`. Projections and the change
 * callback read the EFFECTIVE value via `activeTab(state, config)`.
 */
export type TabsOrientation = 'horizontal' | 'vertical';

export interface TabsConfig {
  /** Controlled active tab: shadows the intrinsic state when present. '' = none. */
  value?: string | undefined;
  /** Uncontrolled seed for the intrinsic active tab. */
  defaultValue?: string | undefined;
  /** Arrow-key navigation axis. Default 'horizontal'. */
  orientation?: TabsOrientation | undefined;
}

export interface TabsState {
  /** Intrinsic active tab -- ignored while a controlled value is present. */
  value: string | null;
}

export type TabsActions = {
  /** Make a tab active (payload: the trigger's value). Tabs never deactivate. */
  activate: string;
};

export type TabsPart = 'root' | 'list' | 'trigger' | 'panel';

/** The effective active tab: a controlled value shadows intrinsic state. */
export function activeTab(state: TabsState, config: TabsConfig): string | null {
  if (config.value !== undefined) return config.value === '' ? null : config.value;
  return state.value;
}

/** Whether a given tab value is the effective active one. */
export function isTabActive(value: string, state: TabsState, config: TabsConfig): boolean {
  return activeTab(state, config) === value;
}

export function orientationOf(config: TabsConfig): TabsOrientation {
  return config.orientation ?? 'horizontal';
}

/**
 * The id naming the three performances share, so a trigger's `aria-controls`
 * and its panel's `aria-labelledby` always resolve to each other. React seeds
 * `baseId` from `useId` and Astro from the author's `id` prop; the DOM-native
 * bind reads the resulting ids back off the markup rather than minting its own
 * (Spec 01: behaviors never generate ids). Sharing the naming here is what
 * keeps the cross-reference identical across all three.
 */
export function tabsIds(baseId: string, value: string): { triggerId: string; panelId: string } {
  return { triggerId: `${baseId}-tab-${value}`, panelId: `${baseId}-panel-${value}` };
}

const tabsSlice: Slice<TabsConfig, TabsState, TabsActions, TabsPart> = {
  name: 'tabs',
  parts: {
    root: {},
    list: { role: 'tablist' },
    trigger: { role: 'tab', many: true },
    // Panels stay in the DOM and toggle `hidden`: panel content must be
    // SSR-stable and present before JS. Presence is constant; visibility is
    // state, projected as the boolean `hidden` on the instance.
    panel: { role: 'tabpanel', many: true },
  },
  initialState: (config) => {
    const seed = config.value ?? config.defaultValue ?? '';
    return { value: seed === '' ? null : seed };
  },
  actions: {
    // Single mode, NOT collapsible: re-activating the active tab keeps it
    // active. Returning the SAME state ref when unchanged means memory does not
    // notify and a controlled consumer's callback does not re-fire -- the same
    // idempotence radio-group's `select` earns, and what makes automatic
    // activation safe to fire on every roving move.
    activate: (state, value) => (state.value === value ? state : { value }),
  },
  aria: (_state, config) => ({
    // The root is an unlabelled wrapper; orientation rides data-* so the view
    // can key off it without the root claiming a role it does not have.
    root: { 'data-orientation': orientationOf(config) },
    list: { 'aria-orientation': orientationOf(config) },
  }),
  // Enter/Space on a trigger activate it. Declared for the pure keymap
  // contract; the DOM binds rely on the native <button> converting Enter/Space
  // to a click (wiring a keydown too would race roving-focus and double-fire).
  // Arrow/Home/End are NOT claimed -- roving-focus owns them for movement, and
  // automatic activation rides its onNavigate callback.
  keymap: (event, _state, part) =>
    part === 'trigger' && (event.key === 'Enter' || event.key === ' ') ? 'activate' : null,
};

/**
 * Per-instance ARIA for the `many` parts (Spec 01: BehaviorSpec.instanceAria).
 * `aria()` projects one AriaAttrs per part NAME; trigger/panel occur once per
 * tab value, so their projections take the instance value and the instance's
 * sibling ids (a trigger reads its panel's id; a panel reads its trigger's).
 *
 * `tabindex` is deliberately absent from the trigger projection: roving-focus
 * owns it as ephemeral DOM state, so it must not appear in a projection the
 * conformance harness asserts against. The panel's `hidden` IS projected, as a
 * boolean the harness asserts by presence -- React writes `hidden=""` where the
 * DOM-native bind writes `hidden="true"`.
 */
export function tabsInstanceAria(
  part: TabsPart,
  value: string,
  state: TabsState,
  config: TabsConfig,
  ids: InstanceIds<TabsPart>,
): AriaAttrs {
  const active = isTabActive(value, state, config);
  if (part === 'trigger') {
    return {
      'aria-selected': active ? 'true' : 'false',
      'aria-controls': ids.panel ?? '',
      'data-state': active ? 'active' : 'inactive',
    };
  }
  if (part === 'panel') {
    return {
      'aria-labelledby': ids.trigger ?? '',
      'data-state': active ? 'active' : 'inactive',
      hidden: active ? undefined : true,
    };
  }
  return {};
}

export const tabs: BehaviorSpec<TabsConfig, TabsState, TabsActions, TabsPart> = {
  ...compose('tabs', tabsSlice),
  instanceAria: tabsInstanceAria,
};

/** Triggers eligible for focus and activation (excludes disabled ones). */
const TRIGGER_SELECTOR = '[data-part="trigger"][data-value]:not([disabled])';

/**
 * Compose the roving-focus primitive for a tab list. Both the DOM-native bind
 * and the React performance call THIS, so automatic activation is written once.
 *
 * Two details are load-bearing, carried over from the oracle controller:
 *   - roving binds to the `[role="tablist"]` element, never the root. Panels
 *     live inside the root, so a root-level keydown listener would move tabs
 *     while focus sits in panel content.
 *   - `currentIndex` is seeded to the active tab's position, so Tab enters the
 *     set at the tab whose panel is showing rather than always at the first.
 */
export function startTabsRoving(
  list: HTMLElement | null,
  orientation: TabsOrientation,
  currentValue: string | null,
  onActivate: (value: string) => void,
): () => void {
  if (!list) return () => {};
  const triggers = Array.from(list.querySelectorAll<HTMLElement>(TRIGGER_SELECTOR));
  const currentIndex = Math.max(
    0,
    triggers.findIndex((trigger) => trigger.dataset['value'] === currentValue),
  );
  return createRovingFocus(list, {
    orientation,
    currentIndex,
    // Automatic activation: moving focus activates the newly focused tab.
    onNavigate: (element) => {
      const value = element.dataset['value'];
      if (value !== undefined) onActivate(value);
    },
  });
}

/**
 * The DOM-native binding of the tabs score -- the client. The Web Component and
 * the Astro <script> both import THIS; only React (retained-mode) reads the
 * projections above declaratively instead. Composes the substrate the same way
 * the React controller does: createBehavior is the model, startTabsRoving
 * composes roving-focus, aria-manager applies the projection, and the DOM is
 * the part registry.
 */
export function bindTabs(root: HTMLElement): () => void {
  const getPart = (part: string): HTMLElement | null =>
    part === 'root' ? root : root.querySelector<HTMLElement>(`[data-part="${part}"]`);

  const list = getPart('list');
  const config: TabsConfig = {
    orientation: list?.getAttribute('aria-orientation') === 'vertical' ? 'vertical' : 'horizontal',
    // Seed the intrinsic value from the server-rendered active trigger. WC and
    // Astro are uncontrolled (no reactive prop), so config.value stays undefined.
    defaultValue:
      root.querySelector<HTMLElement>('[data-part="trigger"][data-state="active"]')?.dataset[
        'value'
      ] ?? '',
  };

  const { memory, dispatch } = createBehavior(tabs, config);
  const request = (value: string): boolean => dispatch('activate', config, value);

  // ids are READ from the markup (server- or author-minted), never generated.
  const ids = {} as PartIds<TabsPart>;
  for (const part of Object.keys(tabs.parts) as TabsPart[]) {
    ids[part] = getPart(part)?.id ?? '';
  }

  // The score's projection is already resolved (final strings, undefined =
  // absent), so apply it raw: validate:false skips aria-manager's author-input
  // coercion, which would re-read the resolved string 'false' as truthy.
  const applyProjection = (el: HTMLElement, attrs: AriaAttrs) => {
    for (const [name, value] of Object.entries(attrs)) {
      updateAriaAttribute(el, name as never, value as never, { validate: false });
    }
  };

  // The `many` parts, resolved from the score's own declaration -- no hardcoded
  // trigger/panel list, so this loop matches the generic harness driver.
  const manyParts = (Object.keys(tabs.parts) as TabsPart[]).filter((part) => tabs.parts[part].many);

  const projectInstances = (state: TabsState) => {
    for (const part of manyParts) {
      for (const el of root.querySelectorAll<HTMLElement>(`[data-part="${part}"]`)) {
        const value = el.dataset['value'];
        if (value === undefined) continue;
        const instanceIds: InstanceIds<TabsPart> = {};
        for (const sibling of manyParts) {
          instanceIds[sibling] =
            root.querySelector<HTMLElement>(`[data-part="${sibling}"][data-value="${value}"]`)
              ?.id ?? '';
        }
        applyProjection(el, tabsInstanceAria(part, value, state, config, instanceIds));
      }
    }
  };

  const render = () => {
    const state = memory.get();
    const projection = tabs.aria(state, config, ids);
    for (const part of Object.keys(projection) as TabsPart[]) {
      const attrs = projection[part];
      const el = getPart(part);
      if (el && attrs) applyProjection(el, attrs);
    }
    projectInstances(state);
  };
  const unsubscribe = memory.subscribe(render); // fires immediately: first paint

  const stopRoving = startTabsRoving(
    list,
    orientationOf(config),
    activeTab(memory.get(), config),
    (value) => {
      request(value);
    },
  );

  // Click activation. Native <button> triggers turn Enter/Space into a click,
  // which is how the score's Enter/Space keymap is fulfilled without a second
  // keydown listener racing roving-focus. Focusing the clicked trigger keeps
  // the roving tabstop on the tab the user just chose (oracle parity).
  const onClick = (event: Event) => {
    const trigger = (event.target as HTMLElement).closest<HTMLElement>(TRIGGER_SELECTOR);
    if (!trigger || !root.contains(trigger)) return;
    const value = trigger.dataset['value'];
    if (value === undefined) return;
    request(value);
    trigger.focus();
  };
  root.addEventListener('click', onClick);

  return () => {
    unsubscribe();
    stopRoving();
    root.removeEventListener('click', onClick);
  };
}
