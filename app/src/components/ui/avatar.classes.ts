import { type AvatarConfig, type AvatarSize, resolveAvatar } from "@/components/ui/avatar.behavior";

/**
 * The view: class strings, no logic. Ported verbatim from the oracle's settled
 * composition. The size map is the only config-dependent piece (root sizing +
 * fallback text scale); the image and fallback structure classes are literals
 * the performances import directly. Semantic colour tokens only
 * (`bg-muted`/`text-muted-foreground`), never a raw colour.
 */

export const avatarBaseClasses = "relative flex shrink-0 overflow-hidden rounded-full";

export const avatarSizeClasses: Record<AvatarSize, string> = {
  xs: "h-6 w-6 text-xs",
  sm: "h-8 w-8 text-sm",
  md: "h-10 w-10 text-base",
  lg: "h-12 w-12 text-lg",
  xl: "h-16 w-16 text-xl",
};

export const avatarImageClasses = "aspect-square h-full w-full object-cover";

export const avatarFallbackClasses =
  "flex h-full w-full items-center justify-center rounded-full bg-muted text-muted-foreground";

export interface AvatarClassSet {
  root: string;
  image: string;
  fallback: string;
}

export function avatarClasses(config: AvatarConfig): AvatarClassSet {
  const { size } = resolveAvatar(config);
  return {
    root: `${avatarBaseClasses} ${avatarSizeClasses[size]}`,
    image: avatarImageClasses,
    fallback: avatarFallbackClasses,
  };
}
