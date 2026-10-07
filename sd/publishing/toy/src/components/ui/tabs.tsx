/**
 * Tabbed interface component with keyboard navigation and ARIA compliance
 *
 * Behavior (selection state, arrow/Home/End navigation, roving tabindex, ARIA and
 * visibility reflection) lives in the framework-agnostic createTabs controller, which
 * composes the shared primitives (selection-group + roving-focus). React renders the
 * markup and delegates to the controller via a callback ref - the same controller the
 * Astro and web-component wrappers use, so behavior cannot drift between frameworks.
 *
 * @cognitive-load 4/10 - Content organization with state management requires cognitive processing
 * @attention-economics Content organization: visible=current context, hidden=available contexts, active=user focus
 * @trust-building Persistent selection, clear active indication, predictable navigation patterns
 * @accessibility Arrow key navigation, tab focus management, panel association, screen reader support
 * @semantic-meaning Structure: tablist=navigation, tab=option, tabpanel=content, selected=current view
 *
 * @usage-patterns
 * DO: Use for related content showing different views of same data/context
 * DO: Provide clear, descriptive, scannable tab names (7±2 maximum)
 * NEVER: More than 7 tabs, unrelated content sections, unclear active state
 *
 * @example
 * ```tsx
 * <Tabs defaultValue="overview">
 *   <Tabs.List>
 *     <Tabs.Trigger value="overview">Overview</Tabs.Trigger>
 *     <Tabs.Trigger value="details">Details</Tabs.Trigger>
 *   </Tabs.List>
 *   <Tabs.Content value="overview">Overview content</Tabs.Content>
 *   <Tabs.Content value="details">Details content</Tabs.Content>
 * </Tabs>
 * ```
 */
import * as React from 'react';
import { useMemory } from '@/hooks/use-memory';
import { createBehavior, type AriaAttrs, type PartIds } from '@/lib/contract';
import classy from '@/lib/primitives/classy';
import {
  activeTab,
  startTabsRoving,
  tabs,
  tabsIds,
  tabsInstanceAria,
  type TabsConfig,
  type TabsOrientation,
  type TabsPart,
  type TabsState,
} from '@/components/ui/tabs.behavior';
import { tabsClasses, type TabsClassSet } from '@/components/ui/tabs.classes';

interface TabsContextValue {
  state: TabsState;
  config: TabsConfig;
  classes: TabsClassSet;
  /** The root's part projection, computed once so children never re-project. */
  aria: Partial<Record<TabsPart, AriaAttrs>>;
  baseId: string;
  request: (value: string) => boolean;
}

const TabsContext = React.createContext<TabsContextValue | null>(null);

function useTabsContext(component: string): TabsContextValue {
  const context = React.useContext(TabsContext);
  if (!context) {
    throw new Error(`${component} must be used within <Tabs>`);
  }
  return context;
}

export interface TabsProps extends React.HTMLAttributes<HTMLDivElement> {
  /** Controlled active tab. */
  value?: string;
  /** Default active tab for uncontrolled usage. */
  defaultValue?: string;
  /** Callback when the active tab changes via user interaction. */
  onValueChange?: (value: string) => void;
  /** Layout / arrow-navigation axis. Default 'horizontal'. */
  orientation?: TabsOrientation;
}

/**
 * Tabbed panels. One panel shows at a time; arrow keys move focus across the
 * triggers and activate as they go (automatic activation), Home/End jump to the
 * ends, and Tab enters the set at the tab whose panel is showing.
 *
 * @cognitive-load 4/10 - decision 1, information 1, interaction 1, disruption 1,
 * learning 0. One decision (which view to see) over a small visible set, but the
 * non-selected content is hidden, so the user must remember what lives behind
 * each label -- the recall cost a radio group does not charge. A universally
 * learned affordance; switching costs one click and is fully reversible.
 * @attention-economics Content organization: the visible panel is the current
 * context, the other labels advertise available contexts, and the active pill
 * marks where the user is. Tabs buy screen real estate by hiding siblings, so
 * the labels must carry the whole scent of what they conceal; past roughly
 * seven tabs the labels stop being scannable and the budget is overdrawn.
 * @trust-building The active tab is always unambiguous (aria-selected plus the
 * data-state pill), selection persists while the user works inside a panel, and
 * navigation is predictable: arrows never skip, never wrap unexpectedly, and
 * never lose the panel the user was reading without showing them the new one.
 * @accessibility role="tablist" with aria-orientation on the rail, role="tab"
 * with aria-selected and aria-controls on every trigger, and role="tabpanel"
 * with aria-labelledby on every panel, all wired to real DOM by the harness.
 * Roving tabindex keeps exactly one trigger in the tab order; inactive panels
 * are `hidden` so AT never reaches stale content; panels are focusable so
 * keyboard users reach panel content directly after choosing a tab.
 */
export function Tabs({
  value,
  defaultValue = '',
  onValueChange,
  orientation = 'horizontal',
  className,
  children,
  ...props
}: TabsProps) {
  const config: TabsConfig = { value, defaultValue, orientation };

  // The controller composes the score with the substrate -- no useBehavior.
  // createBehavior is the model; useMemory subscribes React to it; a useEffect
  // below composes the roving-focus primitive directly against the tab list.
  const { memory, dispatch } = React.useMemo(() => createBehavior(tabs, config), []);
  const state = useMemory(memory);

  const uid = React.useId();
  const ids = React.useMemo(() => {
    const out = {} as PartIds<TabsPart>;
    for (const part of Object.keys(tabs.parts) as TabsPart[]) {
      out[part] = `${uid}-${part}`;
    }
    return out;
  }, [uid]);

  const rootRef = React.useRef<HTMLDivElement>(null);

  // Gotcha #1: the controlled callback compares the EFFECTIVE value before
  // against the INTRINSIC value after the reducer -- a controlled set's
  // effective value never moves (config shadows it), but the callback must
  // still report the value to set. `activate` is idempotent, so the repeated
  // dispatches automatic activation produces never re-fire the callback.
  const latest = React.useRef({ config, onValueChange });
  latest.current = { config, onValueChange };
  const request = React.useCallback(
    (nextValue: string): boolean => {
      const { config: cfg, onValueChange: cb } = latest.current;
      const before = activeTab(memory.get(), cfg) ?? '';
      if (!dispatch('activate', cfg, nextValue)) return false;
      const after = memory.get().value ?? '';
      if (after !== before) cb?.(after);
      return true;
    },
    [memory, dispatch],
  );

  // Compose roving-focus against the TAB LIST, not the root: panels live inside
  // the root, so a root-level listener would move tabs while focus sits in
  // panel content. startTabsRoving is the same composition bindTabs calls, so
  // automatic activation cannot drift between the retained and DOM-native
  // performances. Deliberately not re-run on state change -- that would reset
  // the roving cursor mid-navigation.
  React.useEffect(() => {
    const root = rootRef.current;
    if (!root) return;
    return startTabsRoving(
      root.querySelector<HTMLElement>('[data-part="list"]'),
      orientation,
      activeTab(memory.get(), latest.current.config),
      request,
    );
  }, [orientation, memory, request]);

  const aria = tabs.aria(state, config, ids);
  const classes = tabsClasses(config, state);

  const contextValue: TabsContextValue = { state, config, classes, aria, baseId: uid, request };

  return (
    <TabsContext.Provider value={contextValue}>
      <div
        ref={rootRef}
        data-part="root"
        id={ids.root}
        className={classy(classes.root, className)}
        {...aria.root}
        {...props}
      >
        {children}
      </div>
    </TabsContext.Provider>
  );
}

Tabs.displayName = 'Tabs';

export type TabsListProps = React.HTMLAttributes<HTMLDivElement>;

export function TabsList({ className, children, ...props }: TabsListProps) {
  // Keyboard navigation is owned by the composed roving-focus primitive; this
  // part is the rail it acts on, so the decorator contributes markup only.
  const { classes, aria } = useTabsContext('TabsList');

  return (
    <div
      data-part="list"
      role="tablist"
      className={classy(classes.list, className)}
      {...aria.list}
      {...props}
    >
      {children}
    </div>
  );
}

TabsList.displayName = 'TabsList';

export interface TabsTriggerProps extends React.ButtonHTMLAttributes<HTMLButtonElement> {
  /** Value that identifies this tab. */
  value: string;
}

export function TabsTrigger({
  value,
  className,
  children,
  disabled,
  onClick,
  ...props
}: TabsTriggerProps) {
  const { state, config, classes, baseId, request } = useTabsContext('TabsTrigger');
  const { triggerId, panelId } = tabsIds(baseId, value);
  const aria = tabsInstanceAria('trigger', value, state, config, {
    trigger: triggerId,
    panel: panelId,
  });

  return (
    <button
      type="button"
      role="tab"
      data-part="trigger"
      data-value={value}
      id={triggerId}
      disabled={disabled}
      className={classy(classes.trigger, className)}
      {...aria}
      onClick={(event) => {
        onClick?.(event);
        if (event.defaultPrevented) return;
        request(value);
      }}
      {...props}
    >
      {children}
    </button>
  );
}

TabsTrigger.displayName = 'TabsTrigger';

export interface TabsContentProps extends React.HTMLAttributes<HTMLDivElement> {
  /** Value that identifies the tab this panel belongs to. */
  value: string;
}

export function TabsContent({ value, className, children, ...props }: TabsContentProps) {
  const { state, config, classes, baseId } = useTabsContext('TabsContent');
  const { triggerId, panelId } = tabsIds(baseId, value);
  const aria = tabsInstanceAria('panel', value, state, config, {
    trigger: triggerId,
    panel: panelId,
  });

  return (
    <div
      role="tabpanel"
      data-part="panel"
      data-value={value}
      id={panelId}
      // biome-ignore lint/a11y/noNoninteractiveTabindex: tabpanels are focusable per the WAI-ARIA authoring practices
      tabIndex={0}
      className={classy(classes.panel, className)}
      {...aria}
      {...props}
    >
      {children}
    </div>
  );
}

TabsContent.displayName = 'TabsContent';

// shadcn-compatible namespaced surface, preserved from the oracle.
Tabs.List = TabsList;
Tabs.Trigger = TabsTrigger;
Tabs.Content = TabsContent;

export default Tabs;
