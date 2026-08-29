import { compose } from "@/lib/compose";
import { createBehavior, type AriaAttrs, type BehaviorSpec, type PartIds } from "@/lib/contract";
import { updateAriaAttribute } from "@/lib/primitives/aria-manager";
import { announceToScreenReader } from "@/lib/primitives/sr-announcer";
import {
  pressable,
  type PressableActions,
  type PressableConfig,
  type PressablePart,
  type PressableState,
} from "@/lib/pressable";

export type ButtonVariant =
  | "default"
  | "primary"
  | "secondary"
  | "destructive"
  | "success"
  | "warning"
  | "info"
  | "muted"
  | "accent"
  | "outline"
  | "ghost"
  | "link";

export type ButtonSize =
  | "default"
  | "xs"
  | "sm"
  | "lg"
  | "icon"
  | "icon-xs"
  | "icon-sm"
  | "icon-lg";

export interface ButtonConfig extends PressableConfig {
  variant: ButtonVariant;
  size: ButtonSize;
}

export type ButtonState = PressableState;
export type ButtonActions = PressableActions;
export type ButtonPart = PressablePart;

export const button: BehaviorSpec<ButtonConfig, ButtonState, ButtonActions, ButtonPart> = compose(
  "button",
  pressable<ButtonConfig>(),
);

/**
 * The DOM-native binding of the button score -- the client. The Web Component
 * and the Astro <script> both import THIS; only React (retained-mode) reads the
 * projections declaratively instead. Same shape as bindDialog / bindNavigationMenu:
 * createBehavior is the model, the render composes the sr-announcer primitive
 * directly, aria-manager applies the projection, and the DOM is the part registry.
 *
 * Archetype notes (simple-interactive):
 * - The `root` is a native <button>, so Enter/Space are fulfilled by the
 *   browser as a click (Spec 01 rule 5). The bind wires click -> press ONLY;
 *   there is no keydown branch. A suppressed press (disabled/loading/soft-
 *   disabled, gated by canDispatch) also prevents the click's default so a
 *   form is not submitted and a double activation cannot land.
 * - The loading announcement is edge-triggered: it fires once on the loading
 *   false->true transition, never on a baseline mount (a button whose markup
 *   mounts already-loading projects aria-busy but stays silent). The bind reads
 *   config once from the server/author markup, so loading never transitions
 *   here -- the edge tracking below is the same shape the React controller
 *   uses, and the runtime transition is the retained-mode (React) surface.
 */
export function bindButton(root: HTMLElement): () => void {
  // Config is READ from the projected markup (server- or author-minted), never
  // generated: the same aria attributes the score projects are the inputs the
  // bind re-derives config from -- one source, no dual attributes to drift.
  const config: ButtonConfig = {
    variant: "default",
    size: "default",
    toggle: root.hasAttribute("aria-pressed"),
    defaultPressed: root.getAttribute("aria-pressed") === "true",
    disabled: root.hasAttribute("disabled"),
    softDisabled: root.getAttribute("aria-disabled") === "true",
    loading: root.getAttribute("aria-busy") === "true",
    loadingAnnouncement: root.getAttribute("data-loading-announcement") ?? undefined,
  };

  const getPart = (part: string): HTMLElement | null =>
    part === "root" ? root : root.querySelector<HTMLElement>(`[data-part="${part}"]`);

  const { memory, dispatch } = createBehavior(button, config);

  const request = (action: keyof ButtonActions): boolean => dispatch(action, config);

  // Edge-triggered loading announcement, composing sr-announcer directly. Seed
  // the previous-loading tracker to the baseline config so a mount that is
  // already loading is NOT announced; only a false->true transition fires. The
  // DOM-native bind reads config once, so this never transitions in practice --
  // it holds the same contract the React controller drives at runtime.
  let prevLoading = config.loading ?? false;
  const announceLoadingEdge = (loading: boolean) => {
    if (loading && !prevLoading) {
      announceToScreenReader(config.loadingAnnouncement ?? "Loading", "polite");
    }
    prevLoading = loading;
  };

  // ids are READ from the markup; button carries no id-ref aria, but the
  // projection contract still takes them.
  const ids = {} as PartIds<ButtonPart>;
  for (const part of Object.keys(button.parts) as ButtonPart[]) ids[part] = getPart(part)?.id ?? "";

  // The projection is already resolved (final strings, undefined = absent), so
  // apply it raw: validate:false skips aria-manager's author-input coercion,
  // which would re-interpret the resolved string 'false' as truthy.
  const applyProjection = (el: HTMLElement, attrs: AriaAttrs) => {
    for (const [name, value] of Object.entries(attrs)) {
      updateAriaAttribute(el, name as never, value as never, { validate: false });
    }
  };

  const render = () => {
    const state = memory.get();
    const projection = button.aria(state, config, ids);
    for (const part of Object.keys(projection) as ButtonPart[]) {
      const attrs = projection[part];
      const el = getPart(part);
      if (el && attrs) applyProjection(el, attrs);
    }
    announceLoadingEdge(config.loading ?? false);
  };
  const unsubscribe = memory.subscribe(render); // fires immediately: first paint (baseline)

  // Native <button> converts Enter/Space to click; the click dispatches press.
  // A rejected dispatch (the double-submit / soft-disabled / loading gate)
  // cancels the default so the activation does not escape.
  const onClick = (event: Event) => {
    if (!request("press")) {
      event.preventDefault();
      event.stopPropagation();
    }
  };
  root.addEventListener("click", onClick);

  return () => {
    unsubscribe();
    root.removeEventListener("click", onClick);
  };
}
