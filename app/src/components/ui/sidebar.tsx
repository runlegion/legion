/**
 * Responsive sidebar component for app navigation with collapsible states
 *
 * @cognitive-load 3/10 - Familiar navigation pattern; always visible, predictable location
 * @attention-economics Low attention cost: persistent navigation allows quick orientation
 * @trust-building Consistent location, keyboard toggle (Cmd+B), state persistence
 * @accessibility Keyboard navigation, proper landmarks (nav role), focus management
 * @semantic-meaning Primary navigation: main app sections, user actions, branding
 *
 * @usage-patterns
 * DO: Use for primary app navigation with 4-8 main sections
 * DO: Collapse to icons on mobile/narrow viewports
 * DO: Persist collapsed state in user preferences
 * DO: Include keyboard shortcut for toggle (Cmd+B)
 * DO: Group related items with sections and separators
 * NEVER: Secondary navigation (use tabs or breadcrumbs)
 * NEVER: Temporary content (use Sheet or Drawer)
 * NEVER: More than 2 levels of nesting
 *
 * @example
 * ```tsx
 * <Sidebar.Provider>
 *   <Sidebar>
 *     <Sidebar.Header>
 *       <Logo />
 *     </Sidebar.Header>
 *     <Sidebar.Content>
 *       <Sidebar.Group>
 *         <Sidebar.GroupLabel>Main</Sidebar.GroupLabel>
 *         <Sidebar.Menu>
 *           <Sidebar.MenuItem>
 *             <Sidebar.MenuButton asChild>
 *               <a href="/dashboard">Dashboard</a>
 *             </Sidebar.MenuButton>
 *           </Sidebar.MenuItem>
 *         </Sidebar.Menu>
 *       </Sidebar.Group>
 *     </Sidebar.Content>
 *     <Sidebar.Footer>
 *       <UserMenu />
 *     </Sidebar.Footer>
 *   </Sidebar>
 *   <Sidebar.Inset>
 *     <main>Content here</main>
 *   </Sidebar.Inset>
 * </Sidebar.Provider>
 * ```
 */
import * as React from "react";
import { createBehavior, type AriaAttrs, type PartIds } from "@/lib/contract";
import { keyInputOf } from "@/hooks/key-input";
import { useMemory } from "@/hooks/use-memory";
import classy from "@/lib/primitives/classy";
import { mergeProps } from "@/lib/primitives/slot";
import { Sheet, SheetContent } from "@/components/ui/sheet";
import {
  sidebar,
  toggleIntent,
  type SidebarActions,
  type SidebarConfig,
  type SidebarPart,
  type SidebarSide,
  type SidebarState,
  type SidebarVariant,
} from "@/components/ui/sidebar.behavior";
import {
  sidebarClasses,
  sidebarMenuButtonClasses,
  sidebarMenuSubButtonClasses,
  sidebarPanelClasses,
  type SidebarClassSet,
} from "@/components/ui/sidebar.classes";

interface SidebarContextValue {
  state: SidebarState;
  config: SidebarConfig;
  aria: Partial<Record<SidebarPart, AriaAttrs>>;
  ids: PartIds<SidebarPart>;
  request: (action: keyof SidebarActions) => boolean;
  isMobile: boolean;
  classes: SidebarClassSet;
}

const SidebarContext = React.createContext<SidebarContextValue | null>(null);

export function useSidebar(): SidebarContextValue {
  const context = React.useContext(SidebarContext);
  if (!context) {
    throw new Error("useSidebar must be used within <SidebarProvider>");
  }
  return context;
}

/** The viewport signal -- a browser fact, not application state, so it stays a
 *  local subscription and never enters the score's cell. */
function useIsMobile(): boolean {
  const [isMobile, setIsMobile] = React.useState(false);
  React.useEffect(() => {
    if (typeof window === "undefined" || typeof window.matchMedia !== "function") return;
    const mql = window.matchMedia("(max-width: 768px)");
    setIsMobile(mql.matches);
    const handler = (event: MediaQueryListEvent) => setIsMobile(event.matches);
    mql.addEventListener("change", handler);
    return () => mql.removeEventListener("change", handler);
  }, []);
  return isMobile;
}

export interface SidebarProviderProps extends React.HTMLAttributes<HTMLDivElement> {
  open?: boolean;
  defaultOpen?: boolean;
  onOpenChange?: (open: boolean) => void;
  side?: SidebarSide;
  variant?: SidebarVariant;
  collapsible?: SidebarConfig["collapsible"];
}

export function SidebarProvider({
  open,
  defaultOpen = true,
  onOpenChange,
  side = "left",
  variant = "sidebar",
  collapsible = "offcanvas",
  className,
  children,
  ...props
}: SidebarProviderProps) {
  const config: SidebarConfig = { open, defaultOpen, side, variant, collapsible };
  const isMobile = useIsMobile();

  // The controller composes the score with the substrate -- no useBehavior.
  const { memory, dispatch } = React.useMemo(() => createBehavior(sidebar, config), []);
  const state = useMemory(memory);

  const uid = React.useId();
  const ids = React.useMemo(() => {
    const out = {} as PartIds<SidebarPart>;
    for (const part of Object.keys(sidebar.parts) as SidebarPart[]) out[part] = `${uid}-${part}`;
    return out;
  }, [uid]);

  const latest = React.useRef({ config, onOpenChange });
  latest.current = { config, onOpenChange };
  const request = React.useCallback(
    (action: keyof SidebarActions): boolean => {
      const { config: cfg, onOpenChange: cb } = latest.current;
      if (!dispatch(action, cfg)) return false;
      // Only the desktop expand axis has a controlled callback (the oracle
      // exposed no mobile change prop).
      if (action === "open" || action === "close") cb?.(action === "open");
      return true;
    },
    [dispatch],
  );

  // Persistence is a reaction to the desktop axis, equality-gated (write-only,
  // seeded from defaultOpen -- the oracle never read the cookie back).
  React.useEffect(() => {
    return memory.select(
      (value) => value.open,
      (isDesktopOpen) => {
        if (typeof document === "undefined") return;
        // biome-ignore lint/suspicious/noDocumentCookie: sidebar state persistence across page loads
        document.cookie = `sidebar:state=${isDesktopOpen}; path=/; max-age=${60 * 60 * 24 * 7}`;
      },
    );
  }, [memory]);

  // The global toggle shortcut. Window-scoped (not part-scoped), routed through
  // the score's pure toggleIntent so the routing decision stays in the behavior.
  React.useEffect(() => {
    const handler = (event: KeyboardEvent) => {
      if (event.key.toLowerCase() === "b" && (event.metaKey || event.ctrlKey)) {
        event.preventDefault();
        request(toggleIntent(memory.get(), latest.current.config, isMobile));
      }
    };
    window.addEventListener("keydown", handler);
    return () => window.removeEventListener("keydown", handler);
  }, [memory, request, isMobile]);

  const aria = sidebar.aria(state, config, ids);

  const contextValue: SidebarContextValue = {
    state,
    config,
    aria,
    ids,
    request,
    isMobile,
    classes: sidebarClasses(),
  };

  return (
    <SidebarContext.Provider value={contextValue}>
      <div
        data-part="root"
        data-side={side}
        data-variant={variant}
        className={classy(sidebarClasses().provider, className)}
        {...props}
      >
        {children}
      </div>
    </SidebarContext.Provider>
  );
}

export interface SidebarProps extends React.HTMLAttributes<HTMLElement> {
  side?: SidebarSide;
  variant?: SidebarVariant;
}

export function Sidebar({ side, variant, className, children, onKeyDown, ...props }: SidebarProps) {
  const { state, config, aria, ids, request, isMobile, classes } = useSidebar();
  const resolvedSide = side ?? config.side ?? "left";
  const resolvedVariant = variant ?? config.variant ?? "sidebar";

  // Mobile: the overlay IS our merged Sheet -- a modal dialog (focus-trap,
  // scroll-lock, dismiss-on-outside, Escape, and content UNMOUNTED when closed so
  // its links leave the tab order and a11y tree, WCAG 2.2 AAA). Controlled by the
  // sidebar's openMobile axis; matches shadcn's SidebarProvider architecture.
  if (isMobile) {
    return (
      <Sheet
        open={state.openMobile}
        onOpenChange={(next) => request(next ? "openMobile" : "closeMobile")}
        modal
      >
        <SheetContent
          side={resolvedSide}
          aria-label="Sidebar"
          data-sidebar="sidebar"
          showCloseButton={false}
          className={classy(classes.mobilePanel, className)}
          {...props}
        >
          {children}
        </SheetContent>
      </Sheet>
    );
  }

  // Desktop: the persistent rail. The panel-scoped keymap is kept for symmetry
  // with the bind; on desktop there is no mobile overlay so closeMobile is a
  // gated no-op and Escape is inert (not swallowed).
  const handleKeyDown = (event: React.KeyboardEvent<HTMLElement>) => {
    onKeyDown?.(event);
    if (event.defaultPrevented) return;
    const action = sidebar.keymap(keyInputOf(event), state, "panel", config);
    if (!action) return;
    if (request(action)) event.preventDefault();
  };

  return (
    <nav
      data-part="panel"
      id={ids.panel}
      data-side={resolvedSide}
      data-variant={resolvedVariant}
      className={classy(sidebarPanelClasses(resolvedSide, resolvedVariant), className)}
      onKeyDown={handleKeyDown}
      {...aria.panel}
      {...props}
    >
      {children}
    </nav>
  );
}

export interface SidebarTriggerProps extends React.ButtonHTMLAttributes<HTMLButtonElement> {
  asChild?: boolean;
}

export function SidebarTrigger({
  asChild,
  className,
  onClick,
  children,
  ...props
}: SidebarTriggerProps) {
  const { state, config, aria, ids, request, isMobile, classes } = useSidebar();

  const handleClick = (event: React.MouseEvent<HTMLButtonElement>) => {
    onClick?.(event);
    request(toggleIntent(state, config, isMobile));
  };

  // On mobile the panel is not rendered (the overlay is the portaled Sheet), so
  // the projected aria-controls -> panel id would dangle. Drop it there; the
  // trigger keeps its accessible name and toggles openMobile.
  const partProps = {
    "data-part": "trigger",
    id: ids.trigger,
    className: classy(classes.trigger, className),
    onClick: handleClick,
    ...aria.trigger,
    ...(isMobile ? { "aria-controls": undefined } : {}),
  };

  if (asChild && React.isValidElement(children)) {
    const childProps = children.props as Record<string, unknown>;
    return React.cloneElement(children, mergeProps(partProps, childProps) as React.Attributes);
  }

  return (
    <button type="button" aria-label="Toggle Sidebar" {...partProps} {...props}>
      <svg
        xmlns="http://www.w3.org/2000/svg"
        width="24"
        height="24"
        viewBox="0 0 24 24"
        fill="none"
        stroke="currentColor"
        strokeWidth="2"
        strokeLinecap="round"
        strokeLinejoin="round"
        className="size-4"
        aria-hidden="true"
      >
        <title>Toggle sidebar</title>
        <rect width="18" height="18" x="3" y="3" rx="2" />
        <path d="M9 3v18" />
      </svg>
    </button>
  );
}

export interface SidebarRailProps extends React.ButtonHTMLAttributes<HTMLButtonElement> {}

export function SidebarRail({ className, onClick, ...props }: SidebarRailProps) {
  const { state, config, aria, ids, request, isMobile, classes } = useSidebar();

  const handleClick = (event: React.MouseEvent<HTMLButtonElement>) => {
    onClick?.(event);
    request(toggleIntent(state, config, isMobile));
  };

  return (
    <button
      type="button"
      data-part="rail"
      id={ids.rail}
      tabIndex={-1}
      title="Toggle Sidebar"
      className={classy(classes.rail, className)}
      onClick={handleClick}
      {...aria.rail}
      {...props}
    />
  );
}

export interface SidebarInsetProps extends React.HTMLAttributes<HTMLElement> {}

export function SidebarInset({ className, ...props }: SidebarInsetProps) {
  const { classes } = useSidebar();
  return <main data-part="inset" className={classy(classes.inset, className)} {...props} />;
}

export type SidebarHeaderProps = React.HTMLAttributes<HTMLDivElement>;
export function SidebarHeader({ className, ...props }: SidebarHeaderProps) {
  const { classes } = useSidebar();
  return <div data-sidebar="header" className={classy(classes.header, className)} {...props} />;
}

export type SidebarFooterProps = React.HTMLAttributes<HTMLDivElement>;
export function SidebarFooter({ className, ...props }: SidebarFooterProps) {
  const { classes } = useSidebar();
  return <div data-sidebar="footer" className={classy(classes.footer, className)} {...props} />;
}

export type SidebarContentProps = React.HTMLAttributes<HTMLDivElement>;
export function SidebarContent({ className, ...props }: SidebarContentProps) {
  const { classes } = useSidebar();
  return <div data-sidebar="content" className={classy(classes.content, className)} {...props} />;
}

export type SidebarGroupProps = React.HTMLAttributes<HTMLDivElement>;
export function SidebarGroup({ className, ...props }: SidebarGroupProps) {
  const { classes } = useSidebar();
  return <div data-sidebar="group" className={classy(classes.group, className)} {...props} />;
}

export interface SidebarGroupLabelProps extends React.HTMLAttributes<HTMLDivElement> {
  asChild?: boolean;
}
export function SidebarGroupLabel({
  asChild,
  className,
  children,
  ...props
}: SidebarGroupLabelProps) {
  const { classes } = useSidebar();
  const labelProps = {
    "data-sidebar": "group-label",
    className: classy(classes.groupLabel, className),
    ...props,
  };
  if (asChild && React.isValidElement(children)) {
    const childProps = children.props as Record<string, unknown>;
    return React.cloneElement(children, mergeProps(labelProps, childProps) as React.Attributes);
  }
  return <div {...labelProps}>{children}</div>;
}

export interface SidebarGroupActionProps extends React.ButtonHTMLAttributes<HTMLButtonElement> {
  asChild?: boolean;
}
export function SidebarGroupAction({
  asChild,
  className,
  children,
  ...props
}: SidebarGroupActionProps) {
  const { classes } = useSidebar();
  const actionProps = {
    "data-sidebar": "group-action",
    className: classy(classes.groupAction, className),
    ...props,
  };
  if (asChild && React.isValidElement(children)) {
    const childProps = children.props as Record<string, unknown>;
    return React.cloneElement(children, mergeProps(actionProps, childProps) as React.Attributes);
  }
  return (
    <button type="button" {...actionProps}>
      {children}
    </button>
  );
}

export type SidebarGroupContentProps = React.HTMLAttributes<HTMLDivElement>;
export function SidebarGroupContent({ className, ...props }: SidebarGroupContentProps) {
  const { classes } = useSidebar();
  return (
    <div
      data-sidebar="group-content"
      className={classy(classes.groupContent, className)}
      {...props}
    />
  );
}

export type SidebarMenuProps = React.HTMLAttributes<HTMLUListElement>;
export function SidebarMenu({ className, ...props }: SidebarMenuProps) {
  const { classes } = useSidebar();
  return <ul data-sidebar="menu" className={classy(classes.menu, className)} {...props} />;
}

export type SidebarMenuItemProps = React.HTMLAttributes<HTMLLIElement>;
export function SidebarMenuItem({ className, ...props }: SidebarMenuItemProps) {
  const { classes } = useSidebar();
  return <li data-sidebar="menu-item" className={classy(classes.menuItem, className)} {...props} />;
}

export interface SidebarMenuButtonProps extends React.ButtonHTMLAttributes<HTMLButtonElement> {
  asChild?: boolean;
  isActive?: boolean;
  variant?: "default" | "outline";
  size?: "default" | "sm" | "lg";
}
export function SidebarMenuButton({
  asChild,
  isActive = false,
  variant = "default",
  size = "default",
  className,
  children,
  ...props
}: SidebarMenuButtonProps) {
  const buttonProps = {
    "data-sidebar": "menu-button",
    "data-size": size,
    "data-active": isActive,
    className: classy(sidebarMenuButtonClasses(variant, size), className),
    ...props,
  };
  if (asChild && React.isValidElement(children)) {
    const childProps = children.props as Record<string, unknown>;
    return React.cloneElement(children, mergeProps(buttonProps, childProps) as React.Attributes);
  }
  return (
    <button type="button" {...buttonProps}>
      {children}
    </button>
  );
}

export interface SidebarMenuActionProps extends React.ButtonHTMLAttributes<HTMLButtonElement> {
  asChild?: boolean;
  showOnHover?: boolean;
}
export function SidebarMenuAction({
  asChild,
  showOnHover = false,
  className,
  children,
  ...props
}: SidebarMenuActionProps) {
  const { classes } = useSidebar();
  const actionProps = {
    "data-sidebar": "menu-action",
    className: classy(
      classes.menuAction,
      showOnHover ? classes.menuActionShowOnHover : "",
      className,
    ),
    ...props,
  };
  if (asChild && React.isValidElement(children)) {
    const childProps = children.props as Record<string, unknown>;
    return React.cloneElement(children, mergeProps(actionProps, childProps) as React.Attributes);
  }
  return (
    <button type="button" {...actionProps}>
      {children}
    </button>
  );
}

export type SidebarMenuBadgeProps = React.HTMLAttributes<HTMLDivElement>;
export function SidebarMenuBadge({ className, ...props }: SidebarMenuBadgeProps) {
  const { classes } = useSidebar();
  return (
    <div data-sidebar="menu-badge" className={classy(classes.menuBadge, className)} {...props} />
  );
}

export interface SidebarMenuSkeletonProps extends React.HTMLAttributes<HTMLDivElement> {
  showIcon?: boolean;
}
export function SidebarMenuSkeleton({
  showIcon = false,
  className,
  ...props
}: SidebarMenuSkeletonProps) {
  const { classes } = useSidebar();
  // A stable width per instance so the skeleton does not jitter across renders.
  const [width] = React.useState(() => `${Math.floor(Math.random() * 40) + 50}%`);
  return (
    <div
      data-sidebar="menu-skeleton"
      className={classy(classes.menuSkeleton, className)}
      {...props}
    >
      {showIcon ? <div className={classes.menuSkeletonIcon} /> : null}
      <div
        className={classes.menuSkeletonText}
        style={{ "--skeleton-width": width } as React.CSSProperties}
      />
    </div>
  );
}

export type SidebarMenuSubProps = React.HTMLAttributes<HTMLUListElement>;
export function SidebarMenuSub({ className, ...props }: SidebarMenuSubProps) {
  const { classes } = useSidebar();
  return <ul data-sidebar="menu-sub" className={classy(classes.menuSub, className)} {...props} />;
}

export type SidebarMenuSubItemProps = React.HTMLAttributes<HTMLLIElement>;
export function SidebarMenuSubItem({ className, ...props }: SidebarMenuSubItemProps) {
  const { classes } = useSidebar();
  return (
    <li
      data-sidebar="menu-sub-item"
      className={classy(classes.menuSubItem, className)}
      {...props}
    />
  );
}

export interface SidebarMenuSubButtonProps extends React.AnchorHTMLAttributes<HTMLAnchorElement> {
  asChild?: boolean;
  isActive?: boolean;
  size?: "sm" | "md";
}
export function SidebarMenuSubButton({
  asChild,
  isActive = false,
  size = "md",
  className,
  children,
  ...props
}: SidebarMenuSubButtonProps) {
  const buttonProps = {
    "data-sidebar": "menu-sub-button",
    "data-size": size,
    "data-active": isActive,
    className: classy(sidebarMenuSubButtonClasses(size), className),
    ...props,
  };
  if (asChild && React.isValidElement(children)) {
    const childProps = children.props as Record<string, unknown>;
    return React.cloneElement(children, mergeProps(buttonProps, childProps) as React.Attributes);
  }
  return <a {...buttonProps}>{children}</a>;
}

export type SidebarSeparatorProps = React.HTMLAttributes<HTMLDivElement>;
export function SidebarSeparator({ className, ...props }: SidebarSeparatorProps) {
  const { classes } = useSidebar();
  return (
    <div data-sidebar="separator" className={classy(classes.separator, className)} {...props} />
  );
}

SidebarProvider.displayName = "SidebarProvider";
Sidebar.displayName = "Sidebar";
SidebarTrigger.displayName = "SidebarTrigger";
SidebarRail.displayName = "SidebarRail";
SidebarInset.displayName = "SidebarInset";
SidebarHeader.displayName = "SidebarHeader";
SidebarFooter.displayName = "SidebarFooter";
SidebarContent.displayName = "SidebarContent";
SidebarGroup.displayName = "SidebarGroup";
SidebarGroupLabel.displayName = "SidebarGroupLabel";
SidebarGroupAction.displayName = "SidebarGroupAction";
SidebarGroupContent.displayName = "SidebarGroupContent";
SidebarMenu.displayName = "SidebarMenu";
SidebarMenuItem.displayName = "SidebarMenuItem";
SidebarMenuButton.displayName = "SidebarMenuButton";
SidebarMenuAction.displayName = "SidebarMenuAction";
SidebarMenuBadge.displayName = "SidebarMenuBadge";
SidebarMenuSkeleton.displayName = "SidebarMenuSkeleton";
SidebarMenuSub.displayName = "SidebarMenuSub";
SidebarMenuSubItem.displayName = "SidebarMenuSubItem";
SidebarMenuSubButton.displayName = "SidebarMenuSubButton";
SidebarSeparator.displayName = "SidebarSeparator";

// Namespaced surface (oracle parity: Sidebar.Provider, Sidebar.Trigger, ...).
Sidebar.Provider = SidebarProvider;
Sidebar.Trigger = SidebarTrigger;
Sidebar.Rail = SidebarRail;
Sidebar.Inset = SidebarInset;
Sidebar.Header = SidebarHeader;
Sidebar.Footer = SidebarFooter;
Sidebar.Content = SidebarContent;
Sidebar.Group = SidebarGroup;
Sidebar.GroupLabel = SidebarGroupLabel;
Sidebar.GroupAction = SidebarGroupAction;
Sidebar.GroupContent = SidebarGroupContent;
Sidebar.Menu = SidebarMenu;
Sidebar.MenuItem = SidebarMenuItem;
Sidebar.MenuButton = SidebarMenuButton;
Sidebar.MenuAction = SidebarMenuAction;
Sidebar.MenuBadge = SidebarMenuBadge;
Sidebar.MenuSkeleton = SidebarMenuSkeleton;
Sidebar.MenuSub = SidebarMenuSub;
Sidebar.MenuSubItem = SidebarMenuSubItem;
Sidebar.MenuSubButton = SidebarMenuSubButton;
Sidebar.Separator = SidebarSeparator;
