import type { CommandConfig, CommandState } from "@/components/ui/command.behavior";

export interface CommandClassSet {
  root: string;
  inputWrapper: string;
  inputIcon: string;
  input: string;
  list: string;
  empty: string;
  group: string;
  groupHeading: string;
  item: string;
  separator: string;
  shortcut: string;
  dialogBackdrop: string;
  dialogContent: string;
}

const rootClasses =
  "flex h-full w-full flex-col overflow-hidden rounded-md bg-popover text-popover-foreground";

const inputWrapperClasses = "flex items-center gap-2 border-b px-3";

const inputIconClasses = "size-4 shrink-0 opacity-50";

const inputClasses =
  "flex h-11 @md:h-10 w-full rounded-md bg-transparent py-3 text-body-small ts-body-small outline-none " +
  "placeholder:text-muted-foreground disabled:cursor-not-allowed disabled:opacity-50";

const listClasses = "max-h-80 overflow-y-auto overflow-x-hidden p-1";

const emptyClasses = "py-6 text-center text-body-small ts-body-small";

const groupClasses = "overflow-hidden p-1 text-foreground";

const groupHeadingClasses = "px-2 py-1.5 text-label-small ts-label-small text-muted-foreground";

const itemClasses =
  "relative flex cursor-default select-none items-center gap-2 rounded-sm px-2 py-1.5 " +
  "text-body-small ts-body-small outline-none transition-colors motion-reduce:transition-none " +
  "data-[selected]:bg-accent data-[selected]:text-accent-foreground " +
  "data-[disabled]:pointer-events-none data-[disabled]:opacity-50";

const separatorClasses = "-mx-1 h-px bg-border";

const shortcutClasses = "ml-auto text-shortcut ts-shortcut tracking-widest text-muted-foreground";

// The dialog wrapper is the overlay variant. Enter/exit motion is intentionally
// left undeclared: the semantic motion tokens (motion-modal-in/-out, #1899) do
// not exist yet, and the issue directs leaving motion undeclared over hardcoding
// a raw duration.
const dialogBackdropClasses = "fixed inset-0 z-depth-overlay bg-foreground/80";

const dialogContentClasses =
  "fixed left-1/2 top-1/2 z-depth-modal w-full max-w-lg -translate-x-1/2 -translate-y-1/2 " +
  "overflow-hidden rounded-lg border bg-popover shadow-lg";

export function commandClasses(_config: CommandConfig, _state: CommandState): CommandClassSet {
  return {
    root: rootClasses,
    inputWrapper: inputWrapperClasses,
    inputIcon: inputIconClasses,
    input: inputClasses,
    list: listClasses,
    empty: emptyClasses,
    group: groupClasses,
    groupHeading: groupHeadingClasses,
    item: itemClasses,
    separator: separatorClasses,
    shortcut: shortcutClasses,
    dialogBackdrop: dialogBackdropClasses,
    dialogContent: dialogContentClasses,
  };
}
