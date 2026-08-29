import type { BehaviorSpec } from "@/lib/contract";

/**
 * Avatar: a user's image with an initials (or icon) fallback. A STATIC score
 * in the container/card family -- no actions, no keymap, no effects, and an
 * EMPTY ARIA projection per part (the `<img>` carries its own native `alt`,
 * the root span and the fallback span carry no role). What Avatar adds over a
 * pure static is a datum: an image-load `status`, which -- exactly like
 * Progress's `value` -- is CONFIG, not state. The score is a total function
 * from `status` to which part is present:
 *
 *   loading  image present (loading) + fallback present (shown behind it)
 *   loaded   image present            + fallback HIDDEN
 *   error    image HIDDEN             + fallback present
 *
 * The transition between statuses is a RUNTIME concern the retained-mode
 * (React) performance owns -- an `<img>` `load`/`error` listener flips React
 * state, the same way Button's docstring names the loading transition the
 * React surface. The DOM-native performances (the Web Component and Astro) are
 * caller-decides statics, matching the oracle: the old `avatar.astro` was
 * zero-JS (`src ? <img> : <fallback>`) and the old `avatar.element.ts`
 * explicitly deferred image-load coordination. So there is no universal
 * `bindAvatar`: presence is a pure function of `status`, and each performance
 * applies it -- React from its own state, the WC/Astro from a `status`
 * attribute (defaulted from `src`).
 *
 * `resolveAvatar` is the single computation the classes and all three
 * performances read, so the presence rule and the size resolution live once.
 */

export type AvatarSize = "xs" | "sm" | "md" | "lg" | "xl";
export type AvatarStatus = "loading" | "loaded" | "error";

export interface AvatarConfig {
  /** Size token; unknown values resolve to `md`. */
  size?: AvatarSize | undefined;
  /** Image-load datum. Undefined (or unknown) resolves to `loading`. */
  status?: AvatarStatus | undefined;
}

export type AvatarState = Record<never, never>;
export type AvatarActions = Record<never, never>;
export type AvatarPart = "root" | "image" | "fallback";

export const AVATAR_SIZES: ReadonlyArray<AvatarSize> = ["xs", "sm", "md", "lg", "xl"];
export const AVATAR_STATUSES: ReadonlyArray<AvatarStatus> = ["loading", "loaded", "error"];

export function isAvatarSize(value: string | null | undefined): value is AvatarSize {
  return value != null && (AVATAR_SIZES as ReadonlyArray<string>).includes(value);
}

export function isAvatarStatus(value: string | null | undefined): value is AvatarStatus {
  return value != null && (AVATAR_STATUSES as ReadonlyArray<string>).includes(value);
}

export interface ResolvedAvatar {
  size: AvatarSize;
  status: AvatarStatus;
  /** The image is removed from the tree once it has failed to load. */
  imageHidden: boolean;
  /** The fallback yields to the image once the image has loaded. */
  fallbackHidden: boolean;
}

/**
 * The one computation. `avatar.classes.ts` and every performance read from
 * this so the size resolution and the presence rule live in exactly one place.
 * An unknown size falls back to `md`; an unknown/absent status falls back to
 * `loading` (both parts present), the neutral initial the React state also
 * starts from.
 */
export function resolveAvatar(config: AvatarConfig): ResolvedAvatar {
  const size = isAvatarSize(config.size) ? config.size : "md";
  const status = isAvatarStatus(config.status) ? config.status : "loading";
  return {
    size,
    status,
    imageHidden: status === "error",
    fallbackHidden: status === "loaded",
  };
}

export const avatar: BehaviorSpec<AvatarConfig, AvatarState, AvatarActions, AvatarPart> = {
  name: "avatar",
  // image and fallback are optional: which one is present is a function of
  // status, so neither is guaranteed in a given scenario (boundary: a part the
  // score may omit is declared optional, not many).
  parts: { root: {}, image: { optional: true }, fallback: { optional: true } },
  initialState: () => ({}),
  actions: {},
  canDispatch: () => true,
  // Empty projection, like Container/Card: the image's `alt` is a native
  // passthrough, and the root/fallback spans carry no role. Presence -- not
  // ARIA -- is Avatar's contract, and presence is `resolveAvatar`.
  aria: () => ({ root: {}, image: {}, fallback: {} }),
  keymap: () => null,
};
