import type { InputConfig, InputState } from "@/components/ui/input.behavior";

export interface InputClassSet {
  input: string;
  error: string;
}

// bg-transparent, never a page-fill: the control inherits the surface it sits
// on. Validity is styled off the projected aria-invalid, so light-DOM markup,
// the WC, and React all pick up the destructive border with no extra class.
const inputClasses =
  "flex h-10 w-full rounded-md border border-input bg-transparent px-3 py-2 text-body-small ts-body-small " +
  "text-foreground placeholder:text-muted-foreground " +
  "ring-offset-background " +
  "focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-2 " +
  "transition-shadow duration-100 motion-reduce:transition-none " +
  "disabled:cursor-not-allowed disabled:opacity-50 " +
  "read-only:cursor-default " +
  "aria-invalid:border-destructive aria-invalid:focus-visible:ring-destructive-ring";

const errorClasses = "text-body-small ts-body-small text-destructive";

export function inputClassSet(_config: InputConfig, _state: InputState): InputClassSet {
  return {
    input: inputClasses,
    error: errorClasses,
  };
}
