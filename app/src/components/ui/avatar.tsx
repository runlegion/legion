/**
 * Avatar -- a user's representation: an image with an initials (or icon)
 * fallback on load failure. Compose Avatar with AvatarImage and AvatarFallback;
 * the shadcn/radix surface is preserved exactly (Avatar / AvatarImage /
 * AvatarFallback, `onLoadingStatusChange`, `delayMs`).
 *
 * A static score with a datum: the image-load `status` is CONFIG (like
 * Progress's `value`), and `resolveAvatar` turns it into presence. React owns
 * the runtime transition -- this retained-mode performance holds `status` in
 * state and an `<img>` `load`/`error` listener flips it, then feeds it back
 * through `resolveAvatar` so the score decides which part is present. The
 * `delayMs` fallback-flash guard is a React-only affordance (framework
 * affordance, not a score concern).
 *
 * @cognitive-load 2/10 - decision 0, information 1 (read the person), interaction
 * 0, disruption 0, learning 1. A peripheral display element with predictable
 * behavior; nothing to decide or operate.
 * @attention-economics Peripheral element: supports content identification
 * without demanding focus. An avatar identifies; it never announces. Pair it
 * with a name -- never rely on the image alone to convey who.
 * @trust-building Consistent representation builds recognition across a session;
 * the fallback prevents the broken-image state that erodes polish. Always
 * provide `alt` for a meaningful avatar; a decorative one takes `aria-hidden`.
 * @accessibility The image carries `alt` (native); the root and fallback spans
 * project no ARIA. Meaningful avatars require real alt text; decorative avatars
 * set `aria-hidden`. No keyboard interaction -- Avatar is not a widget.
 * @semantic-meaning Size hierarchy: xs/sm = inline mentions, md = lists,
 * lg/xl = profiles. Match the size to the context.
 *
 * @usage-patterns
 * DO: Always provide alt text for meaningful avatars
 * DO: Use AvatarFallback for graceful degradation
 * DO: Match size to context (small in lists, large in profiles)
 * DO: Use delayMs on the fallback to prevent a loading flash
 * NEVER: Use without a fallback -- images fail
 * NEVER: Rely solely on the avatar for identification -- pair with a name
 * NEVER: Use inconsistent sizes within the same context
 *
 * @example
 * ```tsx
 * <Avatar>
 *   <AvatarImage src="/user.jpg" alt="Jane Doe" />
 *   <AvatarFallback>JD</AvatarFallback>
 * </Avatar>
 * ```
 */
import * as React from "react";
import classy from "@/lib/primitives/classy";
import {
  avatar,
  type AvatarSize,
  type AvatarStatus,
  resolveAvatar,
} from "@/components/ui/avatar.behavior";
import {
  avatarClasses,
  avatarFallbackClasses,
  avatarImageClasses,
} from "@/components/ui/avatar.classes";

export interface AvatarProps extends React.HTMLAttributes<HTMLSpanElement> {
  size?: AvatarSize;
}

export interface AvatarImageProps extends React.ImgHTMLAttributes<HTMLImageElement> {
  onLoadingStatusChange?: (status: AvatarStatus) => void;
}

export interface AvatarFallbackProps extends React.HTMLAttributes<HTMLSpanElement> {
  delayMs?: number;
}

interface AvatarContextValue {
  status: AvatarStatus;
  onStatusChange: (status: AvatarStatus) => void;
}

const AvatarContext = React.createContext<AvatarContextValue | null>(null);

function useAvatarContext(): AvatarContextValue {
  const context = React.useContext(AvatarContext);
  if (!context) {
    throw new Error("Avatar components must be used within an Avatar");
  }
  return context;
}

export const Avatar = React.forwardRef<HTMLSpanElement, AvatarProps>(
  ({ className, size = "md", children, ...props }, ref) => {
    const [status, setStatus] = React.useState<AvatarStatus>("loading");

    const contextValue = React.useMemo<AvatarContextValue>(
      () => ({ status, onStatusChange: setStatus }),
      [status],
    );

    const classes = avatarClasses({ size });
    const aria = avatar.aria({}, { size, status }, { root: "", image: "", fallback: "" });

    return (
      <AvatarContext.Provider value={contextValue}>
        <span
          ref={ref}
          data-part="root"
          className={classy(classes.root, className)}
          {...aria.root}
          {...props}
        >
          {children}
        </span>
      </AvatarContext.Provider>
    );
  },
);

Avatar.displayName = "Avatar";

export const AvatarImage = React.forwardRef<HTMLImageElement, AvatarImageProps>(
  ({ className, src, alt, onLoadingStatusChange, onLoad, onError, ...props }, ref) => {
    const { status, onStatusChange } = useAvatarContext();

    const handleStatusChange = React.useCallback(
      (next: AvatarStatus): void => {
        onStatusChange(next);
        onLoadingStatusChange?.(next);
      },
      [onStatusChange, onLoadingStatusChange],
    );

    // A missing src is an immediate error; a present src (re)enters loading so
    // a changed src re-evaluates. This is the runtime transition React owns.
    React.useEffect(() => {
      handleStatusChange(src ? "loading" : "error");
    }, [src, handleStatusChange]);

    const handleLoad = React.useCallback(
      (event: React.SyntheticEvent<HTMLImageElement>): void => {
        handleStatusChange("loaded");
        onLoad?.(event);
      },
      [handleStatusChange, onLoad],
    );

    const handleError = React.useCallback(
      (event: React.SyntheticEvent<HTMLImageElement>): void => {
        handleStatusChange("error");
        onError?.(event);
      },
      [handleStatusChange, onError],
    );

    // Presence is the score's decision, not the decorator's: the image is
    // removed once it has failed (resolveAvatar.imageHidden).
    if (resolveAvatar({ status }).imageHidden) {
      return null;
    }

    return (
      <img
        ref={ref}
        data-part="image"
        src={src}
        alt={alt}
        className={classy(avatarImageClasses, className)}
        onLoad={handleLoad}
        onError={handleError}
        {...props}
      />
    );
  },
);

AvatarImage.displayName = "AvatarImage";

export const AvatarFallback = React.forwardRef<HTMLSpanElement, AvatarFallbackProps>(
  ({ className, delayMs, children, ...props }, ref) => {
    const { status } = useAvatarContext();
    const [canRender, setCanRender] = React.useState(delayMs === undefined);

    React.useEffect(() => {
      if (delayMs === undefined) return undefined;
      const timer = setTimeout(() => setCanRender(true), delayMs);
      return () => clearTimeout(timer);
    }, [delayMs]);

    // The score yields the fallback once the image has loaded; delayMs is the
    // React-only flash guard layered on top of that decision.
    if (resolveAvatar({ status }).fallbackHidden || !canRender) {
      return null;
    }

    return (
      <span
        ref={ref}
        data-part="fallback"
        className={classy(avatarFallbackClasses, className)}
        {...props}
      >
        {children}
      </span>
    );
  },
);

AvatarFallback.displayName = "AvatarFallback";

export default Avatar;
