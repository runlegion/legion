/**
 * Command component for keyboard-driven command palettes and search interfaces
 *
 * @cognitive-load 6/10 - Command-based interface; requires learning shortcuts but fast once known
 * @attention-economics High initial attention, low ongoing: power users benefit from muscle memory
 * @trust-building Immediate search feedback, keyboard navigable, clear action consequences
 * @accessibility Full keyboard navigation, ARIA combobox pattern, screen reader announcements
 * @semantic-meaning Command execution: quick actions, navigation, search, command palettes
 *
 * @usage-patterns
 * DO: Use for power-user features and keyboard shortcuts
 * DO: Provide instant search/filter feedback
 * DO: Group related commands logically
 * DO: Support both mouse and keyboard navigation
 * DO: Show keyboard shortcut hints
 * NEVER: Use for simple forms or data entry
 * NEVER: Require mouse-only interaction
 * NEVER: Hide without clear dismissal method
 *
 * @example
 * ```tsx
 * <Command>
 *   <Command.Input placeholder="Type a command or search..." />
 *   <Command.List>
 *     <Command.Empty>No results found.</Command.Empty>
 *     <Command.Group heading="Suggestions">
 *       <Command.Item onSelect={() => {}}>Calendar</Command.Item>
 *       <Command.Item onSelect={() => {}}>Search</Command.Item>
 *     </Command.Group>
 *   </Command.List>
 * </Command>
 * ```
 */
import * as React from "react";
import { createBehavior, type AriaAttrs, type PartIds, type PayloadArgs } from "@/lib/contract";
import { keyInputOf } from "@/hooks/key-input";
import { useMemory } from "@/hooks/use-memory";
import classy from "@/lib/primitives/classy";
import {
  command,
  commandItemAria,
  isEmptyShown,
  matchesQuery,
  queryValue,
  startCommandDialogEffects,
  visibleItemValues,
  type CommandActions,
  type CommandConfig,
  type CommandPart,
  type CommandState,
} from "@/components/ui/command.behavior";
import { commandClasses, type CommandClassSet } from "@/components/ui/command.classes";

function sanitize(value: string): string {
  return value.replace(/[^a-zA-Z0-9_-]/g, "-");
}

interface CommandContextValue {
  state: CommandState;
  config: CommandConfig;
  ids: PartIds<CommandPart>;
  aria: Partial<Record<CommandPart, AriaAttrs>>;
  effectiveQuery: string;
  request: <K extends keyof CommandActions>(
    action: K,
    ...payload: PayloadArgs<CommandActions[K]>
  ) => boolean;
  itemId: (value: string) => string;
  classes: CommandClassSet;
}

const CommandContext = React.createContext<CommandContextValue | null>(null);

function useCommandContext(component: string): CommandContextValue {
  const context = React.useContext(CommandContext);
  if (!context) {
    throw new Error(`${component} must be used within <Command>`);
  }
  return context;
}

export interface CommandProps extends Omit<React.HTMLAttributes<HTMLDivElement>, "onSelect"> {
  children: React.ReactNode;
  /** Controlled search query. */
  value?: string;
  /** Uncontrolled seed. */
  defaultValue?: string;
  onValueChange?: (value: string) => void;
  /** Accessible name for the listbox popup. */
  label?: string | undefined;
}

export function Command({
  children,
  value,
  defaultValue = "",
  onValueChange,
  label,
  className,
  ...props
}: CommandProps) {
  const config: CommandConfig = { value, defaultValue, label };

  const { memory, dispatch } = React.useMemo(() => createBehavior(command, config), []);
  const state = useMemory(memory);
  const effectiveQuery = queryValue(state, config);

  const uid = React.useId();
  const ids = React.useMemo(() => {
    const out = {} as PartIds<CommandPart>;
    for (const part of Object.keys(command.parts) as CommandPart[]) out[part] = `${uid}-${part}`;
    return out;
  }, [uid]);

  const itemId = React.useCallback((v: string) => `${uid}-item-${sanitize(v)}`, [uid]);

  const rootRef = React.useRef<HTMLDivElement>(null);

  const latest = React.useRef({ config, onValueChange });
  latest.current = { config, onValueChange };
  const request = React.useCallback(
    <K extends keyof CommandActions>(
      action: K,
      ...payload: PayloadArgs<CommandActions[K]>
    ): boolean => {
      const { config: cfg, onValueChange: onValue } = latest.current;
      // Effective-before vs intrinsic-after: a controlled palette's effective
      // query never moves, but the callback must still report the query to set.
      const queryBefore = queryValue(memory.get(), cfg);
      if (!dispatch(action, cfg, ...payload)) return false;
      const after = memory.get();
      if (after.query !== queryBefore) onValue?.(after.query);
      return true;
    },
    [memory, dispatch],
  );

  // One root-level keydown resolves the focused part and drives the score,
  // mirroring bindCommand, so the input/item view wrappers stay pure adapters.
  const handleKeyDown = (event: React.KeyboardEvent<HTMLDivElement>) => {
    if (event.defaultPrevented) return;
    const partEl = (event.target as HTMLElement).closest<HTMLElement>("[data-part]");
    const part = partEl?.dataset["part"] as CommandPart | undefined;
    if (!part) return;
    const action = command.keymap(keyInputOf(event), state, part, config);
    if (!action) return;
    event.preventDefault();
    if (action === "select") {
      const highlighted = memory.get().highlighted;
      if (highlighted !== undefined) {
        rootRef.current?.querySelector<HTMLElement>(`#${CSS.escape(itemId(highlighted))}`)?.click();
      }
      return;
    }
    const root = rootRef.current;
    if (!root) return;
    const visible = visibleItemValues(root, queryValue(memory.get(), config));
    request(action, visible as never);
  };

  const aria = command.aria(state, config, ids);

  const contextValue: CommandContextValue = {
    state,
    config,
    ids,
    aria,
    effectiveQuery,
    request,
    itemId,
    classes: commandClasses(config, state),
  };

  return (
    <CommandContext.Provider value={contextValue}>
      <div
        ref={rootRef}
        data-part="root"
        id={ids.root}
        data-label={label}
        className={classy(contextValue.classes.root, className)}
        onKeyDown={handleKeyDown}
        {...props}
      >
        {children}
      </div>
    </CommandContext.Provider>
  );
}

export interface CommandDialogProps extends React.HTMLAttributes<HTMLDivElement> {
  open?: boolean;
  onOpenChange?: (open: boolean) => void;
  label?: string;
}

export function CommandDialog({
  open = false,
  onOpenChange,
  label,
  className,
  children,
  ...props
}: CommandDialogProps) {
  const contentRef = React.useRef<HTMLDivElement>(null);

  // The modal-dialog trio (focus-trap + scroll-lock + outside-dismiss) is
  // composed from the score's startCommandDialogEffects -- imitating dialog --
  // level-triggered on the open transition. Escape closes.
  React.useEffect(() => {
    if (!open) return;
    const content = contentRef.current;
    if (!content) return;
    const stop = startCommandDialogEffects({
      content,
      getTrigger: () => null,
      onDismiss: () => onOpenChange?.(false),
    });
    const onKey = (event: KeyboardEvent) => {
      if (event.key === "Escape") onOpenChange?.(false);
    };
    document.addEventListener("keydown", onKey);
    return () => {
      document.removeEventListener("keydown", onKey);
      stop();
    };
  }, [open, onOpenChange]);

  if (!open) return null;

  const classes = commandClasses({}, command.initialState({}));
  return (
    <>
      <button
        type="button"
        aria-label="Close command palette"
        className={classes.dialogBackdrop}
        onClick={() => onOpenChange?.(false)}
      />
      <div ref={contentRef} className={classy(classes.dialogContent, className)} {...props}>
        <Command label={label}>{children}</Command>
      </div>
    </>
  );
}

export type CommandInputProps = Omit<
  React.InputHTMLAttributes<HTMLInputElement>,
  "value" | "onChange"
>;

export function CommandInput({ className, ...props }: CommandInputProps) {
  const { state, ids, aria, effectiveQuery, request, itemId, classes } =
    useCommandContext("CommandInput");

  const activeDescendant = state.highlighted !== undefined ? itemId(state.highlighted) : undefined;

  return (
    <div data-part="input-wrapper" className={classes.inputWrapper}>
      <svg
        xmlns="http://www.w3.org/2000/svg"
        viewBox="0 0 24 24"
        fill="none"
        stroke="currentColor"
        strokeWidth="2"
        strokeLinecap="round"
        strokeLinejoin="round"
        className={classes.inputIcon}
        aria-hidden="true"
      >
        <circle cx="11" cy="11" r="8" />
        <path d="m21 21-4.3-4.3" />
      </svg>
      <input
        data-part="input"
        id={ids.input}
        type="text"
        value={effectiveQuery}
        onChange={(event) => request("setQuery", event.target.value)}
        autoComplete="off"
        autoCorrect="off"
        spellCheck="false"
        aria-activedescendant={activeDescendant}
        {...aria.input}
        {...props}
        className={classy(classes.input, className)}
      />
    </div>
  );
}

export type CommandListProps = React.HTMLAttributes<HTMLDivElement>;

export function CommandList({ className, children, ...props }: CommandListProps) {
  const { ids, aria, classes } = useCommandContext("CommandList");
  return (
    <div
      data-part="list"
      id={ids.list}
      {...aria.list}
      className={classy(classes.list, className)}
      {...props}
    >
      {children}
    </div>
  );
}

export type CommandEmptyProps = React.HTMLAttributes<HTMLDivElement>;

export function CommandEmpty({ className, children, ...props }: CommandEmptyProps) {
  const { effectiveQuery, ids, classes } = useCommandContext("CommandEmpty");
  const ref = React.useRef<HTMLDivElement>(null);
  const [shown, setShown] = React.useState(false);

  // The empty state keys off the live match count -- read from the sibling
  // options in an effect (React-pure: no DOM read in render), matching the
  // bind's isEmptyShown rule.
  React.useEffect(() => {
    const root = ref.current?.closest<HTMLElement>('[data-part="root"]');
    if (!root) return;
    let visible = 0;
    for (const item of root.querySelectorAll<HTMLElement>('[data-part="item"]')) {
      const v = item.dataset["value"];
      if (v === undefined || item.hasAttribute("data-disabled")) continue;
      if (matchesQuery(v, effectiveQuery)) visible++;
    }
    setShown(isEmptyShown(visible, effectiveQuery));
  }, [effectiveQuery]);

  return (
    <div
      ref={ref}
      data-part="empty"
      id={ids.empty}
      hidden={!shown}
      role="presentation"
      className={classy(classes.empty, className)}
      {...props}
    >
      {children}
    </div>
  );
}

export interface CommandGroupProps extends React.HTMLAttributes<HTMLDivElement> {
  heading?: string;
}

export function CommandGroup({ heading, className, children, ...props }: CommandGroupProps) {
  const { classes } = useCommandContext("CommandGroup");
  const headingId = React.useId();
  return (
    // biome-ignore lint/a11y/useSemanticElements: role="group" is the WAI-ARIA APG grouping role for command items
    <div
      role="group"
      aria-labelledby={heading ? headingId : undefined}
      data-part="group"
      className={classy(classes.group, className)}
      {...props}
    >
      {heading &&
        React.createElement(
          "div",
          { id: headingId, "data-part": "group-heading", className: classes.groupHeading },
          heading,
        )}
      {children}
    </div>
  );
}

export interface CommandItemProps extends Omit<React.HTMLAttributes<HTMLDivElement>, "onSelect"> {
  value?: string;
  disabled?: boolean;
  onSelect?: (value: string) => void;
}

export function CommandItem({
  value: itemValue,
  disabled = false,
  onSelect,
  className,
  children,
  onClick,
  onPointerMove,
  ...props
}: CommandItemProps) {
  const { state, config, request, itemId, classes } = useCommandContext("CommandItem");
  const computedValue = itemValue ?? (typeof children === "string" ? children : "");
  const aria = commandItemAria(computedValue, state, config);

  const handleClick = (event: React.MouseEvent<HTMLDivElement>) => {
    onClick?.(event);
    if (event.defaultPrevented || disabled) return;
    request("select", computedValue);
    onSelect?.(computedValue);
  };

  const handlePointerMove = (event: React.PointerEvent<HTMLDivElement>) => {
    onPointerMove?.(event);
    if (!disabled && state.highlighted !== computedValue) request("highlight", computedValue);
  };

  return (
    // biome-ignore lint/a11y/useFocusableInteractive: options are navigated virtually via aria-activedescendant on the input, not individually focusable
    // biome-ignore lint/a11y/useKeyWithClickEvents: keyboard navigation is handled by the command input; the item only reacts to pointer and virtual commit
    <div
      role="option"
      id={itemId(computedValue)}
      data-part="item"
      data-value={computedValue}
      data-disabled={disabled ? "" : undefined}
      aria-disabled={disabled ? "true" : undefined}
      onClick={handleClick}
      onPointerMove={handlePointerMove}
      className={classy(classes.item, className)}
      {...aria}
      {...props}
    >
      {children}
    </div>
  );
}

export type CommandSeparatorProps = React.HTMLAttributes<HTMLDivElement>;

export function CommandSeparator({ className, ...props }: CommandSeparatorProps) {
  const { classes } = useCommandContext("CommandSeparator");
  return (
    <div
      aria-hidden="true"
      data-part="separator"
      className={classy(classes.separator, className)}
      {...props}
    />
  );
}

export type CommandShortcutProps = React.HTMLAttributes<HTMLSpanElement>;

export function CommandShortcut({ className, ...props }: CommandShortcutProps) {
  const { classes } = useCommandContext("CommandShortcut");
  return React.createElement("span", {
    "data-part": "shortcut",
    className: classy(classes.shortcut, className),
    ...props,
  });
}

Command.Dialog = CommandDialog;
Command.Input = CommandInput;
Command.List = CommandList;
Command.Empty = CommandEmpty;
Command.Group = CommandGroup;
Command.Item = CommandItem;
Command.Separator = CommandSeparator;
Command.Shortcut = CommandShortcut;

export { Command as CommandRoot };
