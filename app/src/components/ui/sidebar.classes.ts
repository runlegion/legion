import type { SidebarSide, SidebarVariant } from "@/components/ui/sidebar.behavior";

/**
 * The sidebar view. Class strings only -- no logic, no state reads. The panel
 * carries BOTH axes' data hooks (Spec: one panel, not two): the mobile overlay
 * off-canvas slide keys off `data-mobile`, and the desktop rail collapse keys
 * off `data-state`/`data-collapsible`, each scoped to its breakpoint so a single
 * element serves both viewports.
 *
 * Motion is UNDECLARED. The oracle animated the collapse and the mobile slide
 * with raw `duration-200 ease-linear/ease-in-out`; the horizontal
 * expand/collapse and sheet-slide semantic motion tokens do not exist yet (the
 * token layer is being rebuilt, #1899/#1902), so the from/to states are declared
 * (translate, width) while the timing is left to the future token layer rather
 * than hardcoded. The small `duration-150` interaction acknowledgments on the
 * buttons are kept (Spec 04 retains hover/press feedback -- cf. sheet's close
 * button), never the layout motion.
 */
export interface SidebarClassSet {
  provider: string;
  trigger: string;
  rail: string;
  mobilePanel: string;
  inset: string;
  header: string;
  footer: string;
  content: string;
  group: string;
  groupLabel: string;
  groupAction: string;
  groupContent: string;
  menu: string;
  menuItem: string;
  menuAction: string;
  menuActionShowOnHover: string;
  menuBadge: string;
  menuSkeleton: string;
  menuSkeletonIcon: string;
  menuSkeletonText: string;
  menuSub: string;
  menuSubItem: string;
  separator: string;
}

const providerClasses = "group/sidebar-wrapper flex min-h-svh w-full";

// The one panel. Mobile: a fixed overlay panel (a modal dialog while open; the
// bind `hidden`s it when closed). Desktop (md:): a sticky rail whose width
// collapses. No transition utilities: the layout motion is undeclared (see the
// file header); the enter/exit of the mobile modal is the sheet's concern in
// React, and undeclared for the WC/Astro in-place enhancement.
const panelBaseClasses =
  "flex flex-col bg-sidebar text-sidebar-foreground " +
  "fixed inset-y-0 z-depth-modal h-svh w-72 " +
  "md:sticky md:top-0 md:z-depth-navigation md:h-svh md:w-64 " +
  "md:data-[state=collapsed]:data-[collapsible=icon]:w-12 " +
  "md:data-[state=collapsed]:data-[collapsible=offcanvas]:w-0 " +
  "md:data-[state=collapsed]:data-[collapsible=offcanvas]:overflow-hidden";

const panelSideClasses: Record<SidebarSide, string> = {
  left: "left-0 border-r border-sidebar-border",
  right: "right-0 border-l border-sidebar-border",
};

const panelVariantClasses: Record<SidebarVariant, string> = {
  sidebar: "",
  floating: "md:m-2 md:h-[calc(100svh-theme(spacing.4))] md:rounded-lg md:border md:shadow-sm",
  inset: "md:m-2 md:h-[calc(100svh-theme(spacing.4))] md:rounded-lg md:shadow-sm",
};

/** The panel class: the axis-carrying base plus the anchored side and the
 *  surface variant. The single source of the panel class -- every performance
 *  calls this, so side and variant (decoration the score never touches) are the
 *  only arguments. */
export function sidebarPanelClasses(side: SidebarSide, variant: SidebarVariant): string {
  return `${panelBaseClasses} ${panelSideClasses[side]} ${panelVariantClasses[variant]}`.trim();
}

const triggerClasses = "inline-flex size-7 items-center justify-center";

// The mobile overlay surface: the sidebar fill laid over the merged Sheet's own
// positioning (Sheet owns the modal frame; this only paints the sidebar chrome).
const mobilePanelClasses = "flex h-full w-full flex-col bg-sidebar text-sidebar-foreground";

// The desktop drag rail: a hairline hit target on the collapsing edge. Hidden on
// mobile (the overlay owns dismissal there). The layout slide is undeclared; the
// hover acknowledgment on the hairline is a color swap, not a duration.
const railClasses =
  "absolute inset-y-0 z-depth-navigation hidden w-4 -translate-x-1/2 cursor-ew-resize md:flex " +
  "after:absolute after:inset-y-0 after:left-1/2 after:w-0.5 hover:after:bg-sidebar-border " +
  "group-data-[side=left]:-right-4 group-data-[side=right]:left-0";

const insetClasses = "relative flex min-h-svh w-full flex-1 flex-col bg-background";

const headerClasses = "flex flex-col gap-2 p-2";
const footerClasses = "flex flex-col gap-2 p-2";

const contentClasses =
  "flex min-h-0 flex-1 flex-col gap-2 overflow-auto " +
  "group-data-[collapsible=icon]:overflow-hidden";

const groupClasses = "relative flex w-full min-w-0 flex-col p-2";

const groupLabelClasses =
  "flex h-8 shrink-0 items-center rounded-md px-2 text-label-small ts-label-small text-sidebar-foreground/70 " +
  "outline-none ring-sidebar-ring focus-visible:ring-2 " +
  "group-data-[collapsible=icon]:-mt-8 group-data-[collapsible=icon]:opacity-0";

const groupActionClasses =
  "absolute right-3 top-3.5 flex aspect-square w-5 items-center justify-center rounded-md p-0 " +
  "text-sidebar-foreground outline-none ring-sidebar-ring transition-transform duration-150 " +
  "motion-reduce:transition-none hover:bg-sidebar-accent hover:text-sidebar-accent-foreground " +
  "focus-visible:ring-2 after:absolute after:-inset-2 md:after:hidden " +
  "group-data-[collapsible=icon]:hidden";

const groupContentClasses = "w-full";

const menuClasses = "flex w-full min-w-0 flex-col gap-1";

const menuItemClasses = "group/menu-item relative";

const menuButtonBaseClasses =
  "peer/menu-button flex w-full items-center gap-2 overflow-hidden rounded-md p-2 text-left " +
  "text-label-medium ts-label-medium outline-none ring-sidebar-ring transition-colors duration-150 " +
  "motion-reduce:transition-none hover:bg-sidebar-accent hover:text-sidebar-accent-foreground " +
  "focus-visible:ring-2 active:bg-sidebar-accent active:text-sidebar-accent-foreground " +
  "disabled:pointer-events-none disabled:opacity-50 aria-disabled:pointer-events-none " +
  "aria-disabled:opacity-50 data-[active=true]:bg-sidebar-accent data-[active=true]:font-medium " +
  "data-[active=true]:text-sidebar-accent-foreground group-data-[collapsible=icon]:size-8 " +
  "group-data-[collapsible=icon]:p-2";

const menuButtonSizeClasses: Record<"default" | "sm" | "lg", string> = {
  default: "",
  sm: "text-label-small ts-label-small",
  lg: "text-label-medium ts-label-medium group-data-[collapsible=icon]:p-0",
};

const menuButtonOutlineClasses =
  "bg-background shadow-sm hover:bg-sidebar-accent hover:text-sidebar-accent-foreground hover:shadow-none";

/** The menu button class: base signature plus the size and (optional) outline
 *  variant. */
export function sidebarMenuButtonClasses(
  variant: "default" | "outline" = "default",
  size: "default" | "sm" | "lg" = "default",
): string {
  return [
    menuButtonBaseClasses,
    menuButtonSizeClasses[size],
    variant === "outline" ? menuButtonOutlineClasses : "",
  ]
    .filter(Boolean)
    .join(" ");
}

const menuActionClasses =
  "absolute right-1 top-1.5 flex aspect-square w-5 items-center justify-center rounded-md p-0 " +
  "text-sidebar-foreground outline-none ring-sidebar-ring transition-transform duration-150 " +
  "motion-reduce:transition-none hover:bg-sidebar-accent hover:text-sidebar-accent-foreground " +
  "focus-visible:ring-2 peer-hover/menu-button:text-sidebar-accent-foreground " +
  "after:absolute after:-inset-2 md:after:hidden group-data-[collapsible=icon]:hidden";

const menuActionShowOnHoverClasses =
  "group-focus-within/menu-item:opacity-100 group-hover/menu-item:opacity-100 " +
  "data-[state=open]:opacity-100 peer-data-[active=true]/menu-button:text-sidebar-accent-foreground " +
  "md:opacity-0";

const menuBadgeClasses =
  "pointer-events-none absolute right-1 flex h-5 min-w-5 select-none items-center justify-center " +
  "rounded-md px-1 text-label-small ts-label-small tabular-nums text-sidebar-foreground " +
  "peer-hover/menu-button:text-sidebar-accent-foreground " +
  "peer-data-[active=true]/menu-button:text-sidebar-accent-foreground " +
  "group-data-[collapsible=icon]:hidden";

const menuSkeletonClasses = "flex h-8 items-center gap-2 rounded-md px-2";
const menuSkeletonIconClasses =
  "size-4 shrink-0 animate-pulse motion-reduce:animate-none rounded-md bg-sidebar-accent";
const menuSkeletonTextClasses =
  "h-4 max-w-[--skeleton-width] flex-1 animate-pulse motion-reduce:animate-none rounded-md bg-sidebar-accent";

const menuSubClasses =
  "ml-3.5 flex min-w-0 translate-x-px flex-col gap-1 border-l border-sidebar-border py-0.5 pl-2.5 " +
  "group-data-[collapsible=icon]:hidden";

const menuSubItemClasses = "relative";

const menuSubButtonBaseClasses =
  "flex h-7 min-w-0 -translate-x-px items-center gap-2 overflow-hidden rounded-md px-2 " +
  "text-sidebar-foreground outline-none ring-sidebar-ring transition-colors duration-150 " +
  "motion-reduce:transition-none hover:bg-sidebar-accent hover:text-sidebar-accent-foreground " +
  "focus-visible:ring-2 active:bg-sidebar-accent active:text-sidebar-accent-foreground " +
  "disabled:pointer-events-none disabled:opacity-50 aria-disabled:pointer-events-none " +
  "aria-disabled:opacity-50 data-[active=true]:bg-sidebar-accent " +
  "data-[active=true]:text-sidebar-accent-foreground";

const menuSubButtonSizeClasses: Record<"sm" | "md", string> = {
  sm: "text-label-small ts-label-small",
  md: "text-label-medium ts-label-medium",
};

/** The submenu button class: base signature plus size. */
export function sidebarMenuSubButtonClasses(size: "sm" | "md" = "md"): string {
  return `${menuSubButtonBaseClasses} ${menuSubButtonSizeClasses[size]}`;
}

const separatorClasses = "mx-2 h-px w-auto bg-sidebar-border";

/** The side/variant-independent class set. The panel class is NOT here (it
 *  depends on side + variant, produced by `sidebarPanelClasses`), and the
 *  variant-bearing button classes are their own functions. Everything below is
 *  invariant across side, variant, and state. */
export function sidebarClasses(): SidebarClassSet {
  return {
    provider: providerClasses,
    trigger: triggerClasses,
    rail: railClasses,
    mobilePanel: mobilePanelClasses,
    inset: insetClasses,
    header: headerClasses,
    footer: footerClasses,
    content: contentClasses,
    group: groupClasses,
    groupLabel: groupLabelClasses,
    groupAction: groupActionClasses,
    groupContent: groupContentClasses,
    menu: menuClasses,
    menuItem: menuItemClasses,
    menuAction: menuActionClasses,
    menuActionShowOnHover: menuActionShowOnHoverClasses,
    menuBadge: menuBadgeClasses,
    menuSkeleton: menuSkeletonClasses,
    menuSkeletonIcon: menuSkeletonIconClasses,
    menuSkeletonText: menuSkeletonTextClasses,
    menuSub: menuSubClasses,
    menuSubItem: menuSubItemClasses,
    separator: separatorClasses,
  };
}

export { panelSideClasses, panelVariantClasses };
