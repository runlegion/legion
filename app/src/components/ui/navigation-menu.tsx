/**
 * Navigation menu component for site-level navigation with expandable sections
 *
 * Behavior (which menu is open, arrow-key navigation across triggers, roving tabindex,
 * hover / focus open and close, Escape + outside-click dismiss, and ARIA reflection)
 * lives in the framework-agnostic createNavigationMenu controller, which composes the
 * shared primitives (selection-group + roving-focus + dismissable-layer).
 * React renders structural markup and delegates via a callback ref - the same controller
 * the Astro and web-component wrappers use, so behavior cannot drift between frameworks.
 * VISIBILITY is not in that list any more (#2148): the panel is revealed by the item's
 * own :hover / :focus-within in navigation-menu.classes.ts, over the motion matrix's
 * duration / curve / delay tokens, so it opens with JavaScript turned off.
 *
 * Trigger and Content render NO open-derived attributes that the controller owns once
 * mounted (it reflects aria-expanded / data-state before paint); they render
 * only a static, non-reactive initial state for SSR so the server HTML is correct and
 * re-renders cannot clobber the controller. The decorative Viewport and Indicator are
 * the exception: they subscribe to the controller's open value to size / position
 * themselves, since that chrome has no server-rendered markup to drive it.
 *
 * All Tailwind utilities live in navigation-menu.classes.ts; active / open styling is
 * driven off data-state / data-active set by the controller, not inline utilities.
 *
 * @cognitive-load 5/10 - Navigation requires scanning and decision-making but with predictable patterns
 * @attention-economics Primary navigation: visible structure, expandable sections reveal content on demand
 * @trust-building Predictable hover/click behavior, clear visual indicators, smooth transitions
 * @accessibility Full keyboard support (arrows, escape), proper ARIA navigation role, focus management
 * @semantic-meaning Site navigation with expandable sections for mega-menu style content organization
 *
 * @usage-patterns
 * DO: Use for primary site navigation with grouped content
 * DO: Keep top-level items to 7 or fewer (Miller's Law)
 * DO: Provide clear visual indicator for active/current item
 * DO: Ensure content panels are logically organized
 * DO: Support both hover and click interactions for accessibility
 * NEVER: Use for contextual actions (use DropdownMenu instead)
 * NEVER: Nest more than 2 levels deep
 * NEVER: Hide critical navigation behind expandable sections only
 *
 * @example
 * ```tsx
 * <NavigationMenu>
 *   <NavigationMenu.List>
 *     <NavigationMenu.Item value="products">
 *       <NavigationMenu.Trigger>Products</NavigationMenu.Trigger>
 *       <NavigationMenu.Content>
 *         <NavigationMenu.Link href="/products/widgets">Widgets</NavigationMenu.Link>
 *         <NavigationMenu.Link href="/products/gadgets">Gadgets</NavigationMenu.Link>
 *       </NavigationMenu.Content>
 *     </NavigationMenu.Item>
 *     <NavigationMenu.Item>
 *       <NavigationMenu.Link href="/about">About</NavigationMenu.Link>
 *     </NavigationMenu.Item>
 *   </NavigationMenu.List>
 *   <NavigationMenu.Viewport />
 * </NavigationMenu>
 * ```
 */
import * as React from "react";
import { createBehavior, type PartIds, type PayloadArgs } from "@/lib/contract";
import { keyInputOf } from "@/hooks/key-input";
import { useMemory } from "@/hooks/use-memory";
import classy from "@/lib/primitives/classy";
import { mergeProps } from "@/lib/primitives/slot";
import {
  activeItem,
  navInstanceAria,
  navigationMenu,
  navigationMenuPanel,
  setNavigationMenuDismissed,
  startNavigationMenuEffects,
  type NavigationMenuActions,
  type NavigationMenuConfig,
  type NavigationMenuPart,
  type NavigationMenuState,
} from "@/components/ui/navigation-menu.behavior";
import {
  navigationMenuClasses,
  type NavigationMenuClassSet,
} from "@/components/ui/navigation-menu.classes";

export { navigationMenuTriggerStyle } from "@/components/ui/navigation-menu.classes";

interface NavigationMenuContextValue {
  state: NavigationMenuState;
  ids: PartIds<NavigationMenuPart>;
  aria: Partial<Record<NavigationMenuPart, Record<string, string | boolean | undefined>>>;
  request: <K extends keyof NavigationMenuActions>(
    action: K,
    ...payload: PayloadArgs<NavigationMenuActions[K]>
  ) => boolean;
  getPart: (part: string) => HTMLElement | null;
  /** Toggle an item AND keep the WCAG 1.4.13 dismissal flag in step. */
  toggleItem: (value: string) => void;
  instanceId: (part: NavigationMenuPart, key: string) => string;
  config: NavigationMenuConfig;
  active: string | null;
  classes: NavigationMenuClassSet;
}

const NavigationMenuContext = React.createContext<NavigationMenuContextValue | null>(null);

function useNavigationMenuContext(component: string): NavigationMenuContextValue {
  const context = React.useContext(NavigationMenuContext);
  if (!context) {
    throw new Error(`${component} must be used within <NavigationMenu>`);
  }
  return context;
}

interface NavigationMenuItemContextValue {
  value: string;
  triggerId: string;
  contentId: string;
}

const NavigationMenuItemContext = React.createContext<NavigationMenuItemContextValue | null>(null);

function useItemContext(component: string): NavigationMenuItemContextValue {
  const context = React.useContext(NavigationMenuItemContext);
  if (!context) {
    throw new Error(`${component} must be used within <NavigationMenuItem>`);
  }
  return context;
}

export interface NavigationMenuProps extends React.HTMLAttributes<HTMLElement> {
  value?: string;
  defaultValue?: string;
  onValueChange?: (value: string) => void;
  orientation?: "horizontal" | "vertical";
}

export function NavigationMenu({
  value,
  defaultValue = "",
  onValueChange,
  orientation = "horizontal",
  className,
  children,
  onKeyDown,
  ...props
}: NavigationMenuProps) {
  const config: NavigationMenuConfig = { value, defaultValue, orientation };

  // The controller composes the score with the substrate -- no useBehavior.
  // createBehavior is the model instance (memory + canDispatch-gated
  // dispatch); useMemory subscribes React to it; a mount effect composes the
  // score's roving/hover/dismiss primitives directly. The rest is only what
  // React itself needs: ids (useId) and the dispatch call.
  const { memory, dispatch } = React.useMemo(() => createBehavior(navigationMenu, config), []);
  const state = useMemory(memory);
  const active = activeItem(state, config);

  const uid = React.useId();
  const ids = React.useMemo(() => {
    const out = {} as PartIds<NavigationMenuPart>;
    for (const part of Object.keys(navigationMenu.parts) as NavigationMenuPart[]) {
      out[part] = `${uid}-${part}`;
    }
    return out;
  }, [uid]);
  const instanceId = (part: NavigationMenuPart, key: string) => `${uid}-${part}-${key}`;

  // getPart resolves a part to its element by querying under the root: the
  // DOM is the registry, so there is no ref bookkeeping to keep.
  const rootRef = React.useRef<HTMLElement>(null);
  const getPart = React.useCallback(
    (part: string): HTMLElement | null =>
      part === "root"
        ? rootRef.current
        : (rootRef.current?.querySelector<HTMLElement>(`[data-part="${part}"]`) ?? null),
    [],
  );

  // Effect-initiated dispatches (hover intent resolves after the render that
  // started it) must read the CURRENT config and callback, so those two ride
  // in a ref rather than being captured stale.
  const latest = React.useRef({ config, onValueChange });
  latest.current = { config, onValueChange };
  const request = React.useCallback(
    <K extends keyof NavigationMenuActions>(
      action: K,
      ...payload: PayloadArgs<NavigationMenuActions[K]>
    ): boolean => {
      const { config: cfg, onValueChange: cb } = latest.current;
      // Effective value before vs the INTRINSIC value after: a controlled
      // menu's effective value never moves (config shadows it), but the
      // consumer callback must still report the value it should set next.
      const before = activeItem(memory.get(), cfg) ?? "";
      if (!dispatch(action, cfg, ...payload)) return false;
      const next = memory.get().active ?? "";
      if (next !== before) cb?.(next);
      return true;
    },
    [memory, dispatch],
  );

  // Compose the roving/hover/dismiss trio directly, once per mount (rebuilt
  // only when orientation changes). Every dispatch is immediate, so open/close
  // needs no dependency and roving/hover are not torn down per render. getPart
  // and request are stable.
  React.useEffect(() => {
    const root = rootRef.current;
    if (!root) return;
    return startNavigationMenuEffects({
      root,
      list: getPart("list"),
      orientation,
      onHoverOpen: (value) => void request("hoverOpen", value),
      onClose: () => void request("close"),
    });
  }, [orientation, getPart, request]);

  // A deliberate click is a fresh intent for THIS item, so it drops THIS
  // panel's flag unconditionally (the clicked item is hovered and focused, so
  // the settling sweep in the effects would keep it) -- but a click (or
  // Enter/Space, which a native button fulfils as a click) that CLOSED the panel
  // leaves focus on the trigger, so the item still matches `:focus-within` and
  // the reveal rule would keep the panel visible against a
  // `data-state="closed"`. Raise the same flag the Escape path raises.
  const toggleItem = React.useCallback(
    (value: string) => {
      setNavigationMenuDismissed(navigationMenuPanel(rootRef.current, value), false);
      request("toggle", value);
      if (memory.get().active === null) {
        setNavigationMenuDismissed(navigationMenuPanel(rootRef.current, value), true);
      }
    },
    [memory, request],
  );

  const aria = navigationMenu.aria(state, config, ids);

  const handleKeyDown = (event: React.KeyboardEvent<HTMLElement>) => {
    onKeyDown?.(event);
    if (event.defaultPrevented) return;
    const trigger = (event.target as HTMLElement).closest<HTMLElement>('[data-part="trigger"]');
    const action = navigationMenu.keymap(
      keyInputOf(event),
      state,
      trigger ? "trigger" : "root",
      config,
    );
    if (!action || action === "toggle") return; // native button click dispatches toggle
    if (action === "open") {
      const itemValue = trigger?.dataset["value"];
      if (!itemValue) return;
      event.preventDefault();
      // A deliberate open is a fresh intent for THIS item -- same reason as the
      // click path: the trigger holds focus, so only an unconditional drop gets
      // the panel back up.
      setNavigationMenuDismissed(navigationMenuPanel(rootRef.current, itemValue), false);
      request("open", itemValue);
      return;
    }
    if (action === "close" && active !== null) {
      event.preventDefault();
      const openTrigger = getPart("root")?.querySelector<HTMLElement>(
        `[data-part="trigger"][data-value="${active}"]`,
      );
      request("close");
      // Raise the WCAG dismissal flag on the dismissed PANEL before returning
      // focus: the refocus fires focusin, and the effects' guard reads this
      // attribute to know not to reopen this item (its siblings stay live). It
      // is written imperatively, never rendered, so React's next render cannot
      // clobber what the effects clear.
      setNavigationMenuDismissed(navigationMenuPanel(rootRef.current, active), true);
      openTrigger?.focus();
    }
  };

  const contextValue: NavigationMenuContextValue = {
    state,
    ids,
    aria,
    request,
    getPart,
    toggleItem,
    instanceId,
    config,
    active,
    classes: navigationMenuClasses(config, state),
  };

  return (
    <NavigationMenuContext.Provider value={contextValue}>
      <nav
        ref={rootRef}
        data-part="root"
        id={ids.root}
        className={classy(contextValue.classes.root, className)}
        {...aria.root}
        onKeyDown={handleKeyDown}
        {...props}
      >
        {children}
      </nav>
    </NavigationMenuContext.Provider>
  );
}

export type NavigationMenuListProps = React.HTMLAttributes<HTMLUListElement>;

export function NavigationMenuList({ className, ...props }: NavigationMenuListProps) {
  const { ids, aria, classes } = useNavigationMenuContext("NavigationMenuList");
  return (
    <ul
      data-part="list"
      id={ids.list}
      className={classy(classes.list, className)}
      {...aria.list}
      {...props}
    />
  );
}

export interface NavigationMenuItemProps extends React.LiHTMLAttributes<HTMLLIElement> {
  value: string;
}

export function NavigationMenuItem({ value, className, ...props }: NavigationMenuItemProps) {
  const { instanceId, classes } = useNavigationMenuContext("NavigationMenuItem");
  const itemContext: NavigationMenuItemContextValue = {
    value,
    // Instance ids derived from the root uid -- never hand-templated per call site.
    triggerId: instanceId("trigger", value),
    contentId: instanceId("content", value),
  };
  return (
    <NavigationMenuItemContext.Provider value={itemContext}>
      <li className={classy(classes.item, className)} {...props} />
    </NavigationMenuItemContext.Provider>
  );
}

export type NavigationMenuTriggerProps = React.ButtonHTMLAttributes<HTMLButtonElement>;

export function NavigationMenuTrigger({
  className,
  children,
  onClick,
  ...props
}: NavigationMenuTriggerProps) {
  const { config, state, classes, toggleItem } = useNavigationMenuContext("NavigationMenuTrigger");
  const { value, triggerId, contentId } = useItemContext("NavigationMenuTrigger");
  const aria = navInstanceAria("trigger", value, state, config, {
    trigger: triggerId,
    content: contentId,
  });

  return (
    <button
      type="button"
      id={triggerId}
      data-part="trigger"
      data-value={value}
      data-roving-item
      className={classy(classes.trigger, className)}
      {...aria}
      onClick={(event) => {
        onClick?.(event);
        if (event.defaultPrevented) return;
        toggleItem(value);
      }}
      {...props}
    >
      {children}
      <svg
        xmlns="http://www.w3.org/2000/svg"
        viewBox="0 0 24 24"
        fill="none"
        stroke="currentColor"
        strokeWidth="2"
        strokeLinecap="round"
        strokeLinejoin="round"
        className={classes.triggerChevron}
        aria-hidden="true"
      >
        <path d="m6 9 6 6 6-6" />
      </svg>
    </button>
  );
}

export type NavigationMenuContentProps = React.HTMLAttributes<HTMLDivElement>;

export function NavigationMenuContent({ className, ...props }: NavigationMenuContentProps) {
  const { config, state, classes } = useNavigationMenuContext("NavigationMenuContent");
  const { value, triggerId, contentId } = useItemContext("NavigationMenuContent");
  const aria = navInstanceAria("content", value, state, config, {
    trigger: triggerId,
    content: contentId,
  });

  return (
    <div
      id={contentId}
      data-part="content"
      data-value={value}
      className={classy(classes.content, className)}
      {...aria}
      {...props}
    />
  );
}

export interface NavigationMenuViewportProps extends React.HTMLAttributes<HTMLDivElement> {
  forceMount?: boolean;
}

export function NavigationMenuViewport({
  forceMount,
  className,
  ...props
}: NavigationMenuViewportProps) {
  const { active, ids, aria, classes } = useNavigationMenuContext("NavigationMenuViewport");
  const open = active !== null;
  if (!(forceMount || open)) return null;
  return (
    <div className={classes.viewportWrapper} hidden={open ? undefined : true}>
      <div
        data-part="viewport"
        id={ids.viewport}
        className={classy(classes.viewport, className)}
        {...aria.viewport}
        {...props}
      />
    </div>
  );
}

export interface NavigationMenuIndicatorProps extends React.HTMLAttributes<HTMLDivElement> {
  forceMount?: boolean;
}

export function NavigationMenuIndicator({
  forceMount,
  className,
  ...props
}: NavigationMenuIndicatorProps) {
  const { active, ids, aria, classes } = useNavigationMenuContext("NavigationMenuIndicator");
  const open = active !== null;
  if (!(forceMount || open)) return null;
  return (
    <div
      data-part="indicator"
      id={ids.indicator}
      hidden={open ? undefined : true}
      className={classy(classes.indicator, className)}
      {...aria.indicator}
      {...props}
    >
      <div className={classes.indicatorArrow} />
    </div>
  );
}

export interface NavigationMenuLinkProps extends React.AnchorHTMLAttributes<HTMLAnchorElement> {
  asChild?: boolean;
  active?: boolean;
}

export function NavigationMenuLink({
  asChild,
  active,
  className,
  children,
  ...props
}: NavigationMenuLinkProps) {
  const { classes } = useNavigationMenuContext("NavigationMenuLink");
  const cls = classy(classes.link, className);

  if (asChild && React.isValidElement(children)) {
    const childProps = children.props as Record<string, unknown>;
    const parentProps = { className: cls, "data-active": active || undefined, ...props };
    return React.cloneElement(children, mergeProps(parentProps, childProps) as React.Attributes);
  }

  return (
    <a className={cls} data-active={active || undefined} {...props}>
      {children}
    </a>
  );
}
