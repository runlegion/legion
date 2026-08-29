/**
 * Dropdown selection component with search and accessibility features
 *
 * @cognitive-load 5/10 - Option selection with search functionality requires cognitive processing
 * @attention-economics State management: closed=compact display, open=full options, searching=filtered results
 * @trust-building Search functionality, clear selection indication, undo patterns for accidental selections
 * @accessibility Keyboard navigation, screen reader announcements, focus management, option grouping
 * @semantic-meaning Option structure: value=data, label=display, group=categorization, disabled=unavailable choices
 *
 * @usage-patterns
 * DO: Use 3-12 choices for optimal cognitive load
 * DO: Provide clear, descriptive option text
 * DO: Pre-select most common/safe option when appropriate
 * DO: Enable search for 8+ options to reduce cognitive load
 * NEVER: Too many options without grouping, unclear option descriptions
 *
 * @example
 * ```tsx
 * // shadcn-compatible usage (drop-in replacement)
 * import { Select, SelectTrigger, SelectValue, SelectContent, SelectItem } from '@rafters/ui';
 *
 * <Select>
 *   <SelectTrigger>
 *     <SelectValue placeholder="Choose option..." />
 *   </SelectTrigger>
 *   <SelectContent>
 *     <SelectItem value="option1">Option 1</SelectItem>
 *     <SelectItem value="option2">Option 2</SelectItem>
 *   </SelectContent>
 * </Select>
 *
 * // Or with namespaced imports
 * <Select>
 *   <Select.Trigger>
 *     <Select.Value placeholder="Choose option..." />
 *   </Select.Trigger>
 *   <Select.Content>
 *     <Select.Item value="option1">Option 1</Select.Item>
 *     <Select.Item value="option2">Option 2</Select.Item>
 *   </Select.Content>
 * </Select>
 * ```
 */
import * as React from "react";
import { createBehavior, type AriaAttrs, type PartIds, type PayloadArgs } from "@/lib/contract";
import { keyInputOf } from "@/hooks/key-input";
import { useMemory } from "@/hooks/use-memory";
import classy from "@/lib/primitives/classy";
import { mergeProps } from "@/lib/primitives/slot";
import {
  focusSelectedOption,
  formValueAttrs,
  isOpen,
  select,
  selectItemAria,
  selectedLabel,
  selectedValue,
  startSelectOpenEffects,
  type SelectActions,
  type SelectConfig,
  type SelectPart,
  type SelectState,
} from "@/components/ui/select.behavior";
import { selectClasses, type SelectClassSet } from "@/components/ui/select.classes";

interface SelectContextValue {
  state: SelectState;
  ids: PartIds<SelectPart>;
  aria: Partial<Record<SelectPart, AriaAttrs>>;
  request: <K extends keyof SelectActions>(
    action: K,
    ...payload: PayloadArgs<SelectActions[K]>
  ) => boolean;
  getPart: (part: string) => HTMLElement | null;
  config: SelectConfig;
  effectiveOpen: boolean;
  effectiveValue: string;
  disabled: boolean;
  classes: SelectClassSet;
}

const SelectContext = React.createContext<SelectContextValue | null>(null);

function useSelectContext(component: string): SelectContextValue {
  const context = React.useContext(SelectContext);
  if (!context) {
    throw new Error(`${component} must be used within <Select>`);
  }
  return context;
}

/** Checkmark shown on the selected option. */
function CheckIcon() {
  return (
    <svg
      xmlns="http://www.w3.org/2000/svg"
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth="2"
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden="true"
    >
      <polyline points="20 6 9 17 4 12" />
    </svg>
  );
}

export interface SelectProps {
  children: React.ReactNode;
  value?: string;
  defaultValue?: string;
  onValueChange?: (value: string) => void;
  open?: boolean;
  defaultOpen?: boolean;
  onOpenChange?: (open: boolean) => void;
  disabled?: boolean;
  name?: string;
}

export function Select({
  children,
  value,
  defaultValue = "",
  onValueChange,
  open,
  defaultOpen = false,
  onOpenChange,
  disabled = false,
  name,
}: SelectProps) {
  const config: SelectConfig = { value, defaultValue, open, defaultOpen, disabled, name };

  // The controller composes the score with the substrate -- no useBehavior.
  const { memory, dispatch } = React.useMemo(() => createBehavior(select, config), []);
  const state = useMemory(memory);
  const effectiveOpen = isOpen(state, config);
  const effectiveValue = selectedValue(state, config);

  const uid = React.useId();
  const ids = React.useMemo(() => {
    const out = {} as PartIds<SelectPart>;
    for (const part of Object.keys(select.parts) as SelectPart[]) out[part] = `${uid}-${part}`;
    return out;
  }, [uid]);

  const rootRef = React.useRef<HTMLDivElement>(null);
  const getPart = React.useCallback(
    (part: string): HTMLElement | null =>
      part === "root"
        ? rootRef.current
        : (rootRef.current?.querySelector<HTMLElement>(`[data-part="${part}"]`) ?? null),
    [],
  );

  // Effect-initiated dispatches (outside dismissal) must read the CURRENT
  // config and callbacks, so those ride in a ref rather than captured stale.
  const latest = React.useRef({ config, onValueChange, onOpenChange });
  latest.current = { config, onValueChange, onOpenChange };
  const request = React.useCallback(
    <K extends keyof SelectActions>(
      action: K,
      ...payload: PayloadArgs<SelectActions[K]>
    ): boolean => {
      const { config: cfg, onValueChange: onValue, onOpenChange: onOpen } = latest.current;
      // Effective-before vs intrinsic-after, on BOTH axes: a controlled
      // select's effective value/open never moves, but the callback must still
      // report the value/open it should set next.
      const openBefore = isOpen(memory.get(), cfg);
      const valueBefore = selectedValue(memory.get(), cfg);
      if (!dispatch(action, cfg, ...payload)) return false;
      const after = memory.get();
      if (after.value !== valueBefore) onValue?.(after.value);
      if (after.open !== openBefore) onOpen?.(after.open);
      return true;
    },
    [memory, dispatch],
  );

  // The open-listbox effect trio, composed directly on the open transition
  // (replacing the effects runner): roving focus, typeahead, and outside
  // dismissal sparing the trigger. Level-triggered via the dependency array;
  // declared ABOVE the open-focus effect so roving sets the roving tabindex
  // before focusSelectedOption lands focus. getPart/request are stable, so the
  // listeners are torn down and rebuilt only on the open transition.
  React.useEffect(() => {
    if (!effectiveOpen) return;
    const content = getPart("content");
    if (!content) return;
    return startSelectOpenEffects({
      content,
      getTrigger: () => getPart("trigger"),
      onDismiss: () => {
        request("close");
      },
    });
  }, [effectiveOpen, getPart, request]);

  // Open-focus: land on the selected (or first) option when the listbox opens
  // and focus is not already inside it -- the same rule bindSelect runs.
  React.useEffect(() => {
    if (!effectiveOpen) return;
    const content = getPart("content");
    if (content && !content.contains(document.activeElement)) {
      focusSelectedOption(content, effectiveValue);
    }
  }, [effectiveOpen, effectiveValue, getPart]);

  // One root-level keydown handler resolves the focused part and drives the
  // score, mirroring bindSelect -- so the trigger/item view wrappers stay pure
  // click/pointer/focus adapters with no keymap logic of their own.
  const handleKeyDown = (event: React.KeyboardEvent<HTMLDivElement>) => {
    if (event.defaultPrevented) return;
    const partEl = (event.target as HTMLElement).closest<HTMLElement>("[data-part]");
    const part = partEl?.dataset["part"] as SelectPart | undefined;
    if (!part) return;
    const action = select.keymap(keyInputOf(event), state, part, config);
    if (!action) return;
    if (action === "open") {
      if (disabled) return;
      // preventDefault suppresses the native button click Enter/Space would
      // otherwise fire (which would toggle back closed).
      event.preventDefault();
      request("open");
      return;
    }
    if (action === "close") {
      event.preventDefault();
      request("close");
      getPart("trigger")?.focus();
      return;
    }
    if (action === "select") {
      const item = partEl?.closest<HTMLElement>('[data-part="item"]');
      const itemValue = item?.dataset["value"];
      if (itemValue !== undefined && item && item.getAttribute("aria-disabled") !== "true") {
        event.preventDefault();
        request("select", itemValue);
        getPart("trigger")?.focus();
      }
    }
  };

  const aria = select.aria(state, config, ids);

  const contextValue: SelectContextValue = {
    state,
    ids,
    aria,
    request,
    getPart,
    config,
    effectiveOpen,
    effectiveValue,
    disabled,
    classes: selectClasses(config, state),
  };

  return (
    <SelectContext.Provider value={contextValue}>
      <div ref={rootRef} data-part="root" id={ids.root} {...aria.root} onKeyDown={handleKeyDown}>
        {children}
        {/* Form association composed through the form-value primitive. */}
        {(() => {
          const hidden = formValueAttrs({ name, value: effectiveValue });
          return hidden ? <input data-part="hidden-input" {...hidden} readOnly /> : null;
        })()}
      </div>
    </SelectContext.Provider>
  );
}

export interface SelectTriggerProps extends React.ButtonHTMLAttributes<HTMLButtonElement> {
  asChild?: boolean;
}

export function SelectTrigger({
  className,
  children,
  asChild,
  onClick,
  ...props
}: SelectTriggerProps) {
  const { ids, aria, request, effectiveOpen, disabled, classes } =
    useSelectContext("SelectTrigger");

  const handleClick = (event: React.MouseEvent<HTMLButtonElement>) => {
    onClick?.(event);
    if (event.defaultPrevented || disabled) return;
    request(effectiveOpen ? "close" : "open");
  };

  const partProps = {
    "data-part": "trigger",
    id: ids.trigger,
    disabled,
    ...aria.trigger,
    onClick: handleClick,
  };

  if (asChild && React.isValidElement(children)) {
    const childProps = children.props as Record<string, unknown>;
    return React.cloneElement(children, mergeProps(partProps, childProps) as React.Attributes);
  }

  return (
    <button type="button" className={classy(classes.trigger, className)} {...partProps} {...props}>
      {children}
      <svg
        xmlns="http://www.w3.org/2000/svg"
        viewBox="0 0 24 24"
        fill="none"
        stroke="currentColor"
        strokeWidth="2"
        strokeLinecap="round"
        strokeLinejoin="round"
        className={classes.chevron}
        aria-hidden="true"
      >
        <path d="m6 9 6 6 6-6" />
      </svg>
    </button>
  );
}

export interface SelectValueProps extends React.HTMLAttributes<HTMLSpanElement> {
  placeholder?: string;
}

export function SelectValue({ placeholder, className, children, ...props }: SelectValueProps) {
  const { effectiveValue, getPart, classes } = useSelectContext("SelectValue");
  // Read the selected option's label from the light-DOM listbox after commit
  // (React-pure: DOM read lives in an effect, never in render).
  const [label, setLabel] = React.useState<string | undefined>(undefined);
  React.useEffect(() => {
    setLabel(selectedLabel(getPart("content"), effectiveValue));
  }, [effectiveValue, getPart]);

  const hasValue = effectiveValue !== "";
  const display = children ?? (hasValue ? (label ?? effectiveValue) : placeholder);

  return React.createElement(
    "span",
    {
      "data-part": "value",
      "data-placeholder": placeholder,
      "data-empty": hasValue ? undefined : "",
      className: classy(classes.value, className),
      ...props,
    },
    display,
  );
}

/** Kept for shadcn drop-in compatibility. In the behavior-layer model the
 *  listbox lives in light DOM (present-but-hidden), so there is no portal to
 *  open; this is a pass-through that preserves the API. */
export function SelectPortal({ children }: { children: React.ReactNode }) {
  return <>{children}</>;
}

export interface SelectContentProps extends React.HTMLAttributes<HTMLDivElement> {
  asChild?: boolean;
}

export function SelectContent({ className, children, asChild, ...props }: SelectContentProps) {
  const { ids, aria, effectiveOpen, classes } = useSelectContext("SelectContent");

  const partProps = {
    "data-part": "content",
    id: ids.content,
    hidden: effectiveOpen ? undefined : true,
    ...aria.content,
  };

  if (asChild && React.isValidElement(children)) {
    const childProps = children.props as Record<string, unknown>;
    return React.cloneElement(children, mergeProps(partProps, childProps) as React.Attributes);
  }

  return (
    <div className={classy(classes.content, className)} {...partProps} {...props}>
      <div className={classes.viewport}>{children}</div>
    </div>
  );
}

export type SelectViewportProps = React.HTMLAttributes<HTMLDivElement>;

export function SelectViewport({ className, ...props }: SelectViewportProps) {
  const { classes } = useSelectContext("SelectViewport");
  return <div className={classy(classes.viewport, className)} {...props} />;
}

export type SelectGroupProps = React.HTMLAttributes<HTMLDivElement>;

export function SelectGroup({ className, ...props }: SelectGroupProps) {
  const { classes } = useSelectContext("SelectGroup");
  // biome-ignore lint/a11y/useSemanticElements: role="group" is correct for option groups per WAI-ARIA APG
  return <div role="group" className={classy(classes.group, className)} {...props} />;
}

export type SelectLabelProps = React.HTMLAttributes<HTMLDivElement>;

export function SelectLabel({ className, ...props }: SelectLabelProps) {
  const { classes } = useSelectContext("SelectLabel");
  return <div className={classy(classes.label, className)} {...props} />;
}

export interface SelectItemProps extends React.HTMLAttributes<HTMLDivElement> {
  value: string;
  disabled?: boolean;
  asChild?: boolean;
}

export function SelectItem({
  className,
  children,
  value: itemValue,
  disabled = false,
  onClick,
  onPointerMove,
  onFocus,
  ...props
}: SelectItemProps) {
  const { state, config, request, getPart, classes } = useSelectContext("SelectItem");
  const aria = selectItemAria(itemValue, state, config);
  const isSelected = selectedValue(state, config) === itemValue;

  const handleClick = (event: React.MouseEvent<HTMLDivElement>) => {
    onClick?.(event);
    if (event.defaultPrevented || disabled) return;
    request("select", itemValue);
    getPart("trigger")?.focus();
  };

  const handlePointerMove = (event: React.PointerEvent<HTMLDivElement>) => {
    onPointerMove?.(event);
    if (!disabled) event.currentTarget.focus();
  };

  const handleFocus = (event: React.FocusEvent<HTMLDivElement>) => {
    onFocus?.(event);
    if (!disabled) request("highlight", itemValue);
  };

  const indicator = React.createElement(
    "span",
    { className: classes.itemIndicator },
    isSelected ? <CheckIcon /> : null,
  );
  const text = React.createElement("span", { className: classes.itemText }, children);

  return (
    // biome-ignore lint/a11y/useSemanticElements: role="option" is the listbox APG pattern
    <div
      role="option"
      data-part="item"
      data-value={itemValue}
      data-roving-item=""
      data-disabled={disabled ? "" : undefined}
      aria-disabled={disabled ? "true" : undefined}
      tabIndex={disabled ? undefined : -1}
      className={classy(classes.item, className)}
      {...aria}
      onClick={handleClick}
      onPointerMove={handlePointerMove}
      onFocus={handleFocus}
      {...props}
    >
      {indicator}
      {text}
    </div>
  );
}

export type SelectSeparatorProps = React.HTMLAttributes<HTMLDivElement>;

export function SelectSeparator({ className, ...props }: SelectSeparatorProps) {
  const { classes } = useSelectContext("SelectSeparator");
  return <div aria-hidden="true" className={classy(classes.separator, className)} {...props} />;
}

export type SelectScrollButtonProps = React.HTMLAttributes<HTMLDivElement>;

function ScrollIcon({ path, className }: { path: string; className: string }) {
  return (
    <svg
      xmlns="http://www.w3.org/2000/svg"
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth="2"
      strokeLinecap="round"
      strokeLinejoin="round"
      className={className}
      aria-hidden="true"
    >
      <path d={path} />
    </svg>
  );
}

export function SelectScrollUpButton({ className, children, ...props }: SelectScrollButtonProps) {
  const { classes } = useSelectContext("SelectScrollUpButton");
  return (
    <div aria-hidden="true" className={classy(classes.scrollButton, className)} {...props}>
      {children ?? <ScrollIcon path="m18 15-6-6-6 6" className={classes.scrollIcon} />}
    </div>
  );
}

export function SelectScrollDownButton({ className, children, ...props }: SelectScrollButtonProps) {
  const { classes } = useSelectContext("SelectScrollDownButton");
  return (
    <div aria-hidden="true" className={classy(classes.scrollButton, className)} {...props}>
      {children ?? <ScrollIcon path="m6 9 6 6 6-6" className={classes.scrollIcon} />}
    </div>
  );
}

export type SelectIconProps = React.HTMLAttributes<HTMLSpanElement>;

/** @deprecated The chevron is included in SelectTrigger automatically. Kept
 *  for shadcn drop-in compatibility. */
export function SelectIcon({ className, children, ...props }: SelectIconProps) {
  return React.createElement("span", { "aria-hidden": "true", className, ...props }, children);
}

Select.Trigger = SelectTrigger;
Select.Value = SelectValue;
Select.Portal = SelectPortal;
Select.Content = SelectContent;
Select.Viewport = SelectViewport;
Select.Group = SelectGroup;
Select.Label = SelectLabel;
Select.Item = SelectItem;
Select.Separator = SelectSeparator;
Select.ScrollUpButton = SelectScrollUpButton;
Select.ScrollDownButton = SelectScrollDownButton;
Select.Icon = SelectIcon;

export { Select as SelectRoot };
