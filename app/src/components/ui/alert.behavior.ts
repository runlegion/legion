import type { BehaviorSpec } from "@/lib/contract";

/**
 * Alert: an inline status banner. A static score -- no state, no actions,
 * no keymap, no effects. Its contract is structural: the root always
 * carries `role="alert"` (an assistive-tech live region that announces on
 * appearance -- unlike Container's landmarks, this role is NOT native to
 * `div`, so the score projects it rather than leaving it to element choice),
 * and the severity variant drives token-driven classes only.
 */

export type AlertVariant =
  | "default"
  | "primary"
  | "secondary"
  | "destructive"
  | "success"
  | "warning"
  | "info"
  | "muted"
  | "accent";

export interface AlertConfig {
  variant?: AlertVariant | undefined;
}

export type AlertState = Record<never, never>;
export type AlertActions = Record<never, never>;
export type AlertPart = "root";

export const alert: BehaviorSpec<AlertConfig, AlertState, AlertActions, AlertPart> = {
  name: "alert",
  parts: { root: {} },
  initialState: () => ({}),
  actions: {},
  canDispatch: () => true,
  // role=alert is the whole contract: an assertive live region, projected
  // unconditionally (never native to div, so the score must state it).
  aria: () => ({ root: { role: "alert" } }),
  keymap: () => null,
};
