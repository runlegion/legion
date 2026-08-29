import type { AriaAttrs, PartDecl } from "@/lib/contract";
import type { Slice } from "@/lib/compose";

export interface PressableConfig {
  toggle?: boolean | undefined;
  defaultPressed?: boolean | undefined;
  loadingAnnouncement?: string | undefined;
  loadedAnnouncement?: string | undefined;
  disabled?: boolean | undefined;
  softDisabled?: boolean | undefined;
  loading?: boolean | undefined;
}

export interface PressableState {
  pressed: boolean | undefined;
}

export type PressableActions = {
  press: undefined;
};

export type PressablePart = "root" | "label" | "spinner";

const parts: Record<PressablePart, PartDecl> = {
  root: {},
  label: {},
  spinner: { optional: true },
};

export function pressable<Config extends PressableConfig>(): Slice<
  Config,
  PressableState,
  PressableActions,
  PressablePart
> {
  return {
    name: "pressable",
    parts,
    initialState: (config) => ({
      pressed: config.toggle ? (config.defaultPressed ?? false) : undefined,
    }),
    actions: {
      press: (state) =>
        state.pressed === undefined ? state : { ...state, pressed: !state.pressed },
    },
    canDispatch: (_state, action, config) =>
      action === "press" ? !(config.disabled || config.softDisabled || config.loading) : true,
    aria: (state, config) => {
      const root: AriaAttrs = {
        "aria-busy": config.loading ? "true" : undefined,
        "aria-disabled": config.softDisabled && !config.disabled ? "true" : undefined,
        "aria-pressed": state.pressed === undefined ? undefined : state.pressed ? "true" : "false",
        "data-state": config.loading ? "loading" : config.softDisabled ? "soft-disabled" : "idle",
      };
      return {
        root,
        spinner: { "aria-hidden": "true" },
      };
    },
    keymap: (event, _state, part) =>
      part === "root" && (event.key === "Enter" || event.key === " ") ? "press" : null,
  };
}
