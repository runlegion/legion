import type { BehaviorSpec } from '@/lib/contract';

/**
 * Alert: an inline status banner. A static score -- no state, no actions,
 * no keymap, no effects. Its contract is structural: the root always
 * carries `role="alert"` (an assistive-tech live region that announces on
 * appearance -- unlike Container's landmarks, this role is NOT native to
 * `div`, so the score projects it rather than leaving it to element choice),
 * and the severity variant drives token-driven classes only.
 */

/**
 * The severity vocabulary, in runtime form so nothing has to restate it.
 * DOM-native performances take the variant as `string | null` and must narrow
 * before they can call `alertClasses`, and the test lanes enumerate it to build
 * one scene per variant. Deriving the type from the array rather than declaring
 * both keeps them structurally impossible to desync -- a hand-kept parallel
 * list is exactly how a variant goes missing from one lane and nobody notices.
 */
export const ALERT_VARIANTS = [
  'default',
  'primary',
  'secondary',
  'destructive',
  'success',
  'warning',
  'info',
  'muted',
  'accent',
] as const;

export type AlertVariant = (typeof ALERT_VARIANTS)[number];

export function isAlertVariant(value: string | null | undefined): value is AlertVariant {
  return value != null && (ALERT_VARIANTS as ReadonlyArray<string>).includes(value);
}

export interface AlertConfig {
  variant?: AlertVariant | undefined;
}

export type AlertState = Record<never, never>;
export type AlertActions = Record<never, never>;
export type AlertPart = 'root';

export const alert: BehaviorSpec<AlertConfig, AlertState, AlertActions, AlertPart> = {
  name: 'alert',
  parts: { root: {} },
  initialState: () => ({}),
  actions: {},
  canDispatch: () => true,
  // role=alert is the whole contract: an assertive live region, projected
  // unconditionally (never native to div, so the score must state it).
  aria: () => ({ root: { role: 'alert' } }),
  keymap: () => null,
};
