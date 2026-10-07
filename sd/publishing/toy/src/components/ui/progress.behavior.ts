import { createBehavior, type AriaAttrs, type BehaviorSpec } from '@/lib/contract';
import { updateAriaAttribute } from '@/lib/primitives/aria-manager';

/**
 * Progress: a determinate/indeterminate activity meter. A STATIC score -- no
 * state, no actions, no keymap, no effects -- but unlike Container/Card its
 * ARIA projection is LIVE: the `root` part carries `role="progressbar"` and
 * the aria-value contract, so the harness audits the projection here.
 *
 * `value` is CONFIG, not state: it is the consumer's datum, immutable from the
 * score's point of view (the WC re-reads it on attribute change; React
 * re-renders on prop change). The score is a total function from config to the
 * progressbar's attributes.
 *
 * ARIA disposition (the three oracle a11y approaches unified): the old React
 * and Astro targets rendered an sr-only native `<progress>` PLUS a visual div;
 * the old Web Component used `role="progressbar"` with aria-value*. The
 * behavior layer projects ONE progressbar (`role="progressbar"` +
 * aria-valuemin/max/now/text) across all three -- the WC oracle's approach,
 * which is the equivalent screen-reader semantic without a duplicate node.
 *
 * Indeterminate (value absent or non-finite): the progressbar omits
 * aria-valuenow/aria-valuetext (the ARIA signal for indeterminate) and carries
 * aria-busy="true"; the indicator rides the shared indeterminate animation
 * utility (a CLASS, composed by progress.classes.ts, not projected here).
 */

export type ProgressVariant =
  | 'default'
  | 'primary'
  | 'secondary'
  | 'destructive'
  | 'success'
  | 'warning'
  | 'info'
  | 'accent';

export type ProgressSize = 'sm' | 'default' | 'lg';

export interface ProgressConfig {
  /** Current value in [0, max]. Undefined (or non-finite) = indeterminate. */
  value?: number | undefined;
  /** Maximum value. Non-numeric or non-positive falls back to 100. */
  max?: number | undefined;
  /** Resolved accessible label; overrides the default `${percent}%`. The
   *  React `getValueLabel` and the WC/Astro `value-text` attribute both feed
   *  this -- the score never formats beyond the percentage default. */
  valueText?: string | undefined;
  variant?: ProgressVariant | undefined;
  size?: ProgressSize | undefined;
}

export type ProgressState = Record<never, never>;
export type ProgressActions = Record<never, never>;
export type ProgressPart = 'root' | 'indicator';

const ALLOWED_VARIANTS: ReadonlyArray<ProgressVariant> = [
  'default',
  'primary',
  'secondary',
  'destructive',
  'success',
  'warning',
  'info',
  'accent',
];

const ALLOWED_SIZES: ReadonlyArray<ProgressSize> = ['sm', 'default', 'lg'];

export interface ResolvedProgress {
  indeterminate: boolean;
  clamped: number;
  max: number;
  /** Raw fill percentage (unrounded) -- the indicator width. */
  percent: number;
  /** Accessible label; undefined while indeterminate. */
  valueText: string | undefined;
}

/**
 * The one computation. aria(), progress.classes.ts and bindProgress all read
 * from this -- the single source the three performances apply.
 */
export function resolveProgress(config: ProgressConfig): ResolvedProgress {
  const rawMax = config.max;
  const max = typeof rawMax === 'number' && Number.isFinite(rawMax) && rawMax > 0 ? rawMax : 100;

  const value = config.value;
  if (value === undefined || !Number.isFinite(value)) {
    return { indeterminate: true, clamped: 0, max, percent: 0, valueText: undefined };
  }

  const clamped = Math.min(Math.max(value, 0), max);
  const percent = (clamped / max) * 100;
  const valueText = config.valueText ?? `${Math.round(percent)}%`;
  return { indeterminate: false, clamped, max, percent, valueText };
}

export const progress: BehaviorSpec<ProgressConfig, ProgressState, ProgressActions, ProgressPart> =
  {
    name: 'progress',
    parts: { root: {}, indicator: {} },
    initialState: () => ({}),
    actions: {},
    canDispatch: () => true,
    aria: (_state, config) => {
      const { indeterminate, clamped, max, valueText } = resolveProgress(config);
      return {
        root: {
          role: 'progressbar',
          'aria-valuemin': '0',
          'aria-valuemax': String(max),
          'aria-valuenow': indeterminate ? undefined : String(clamped),
          'aria-valuetext': indeterminate ? undefined : valueText,
          'aria-busy': indeterminate ? 'true' : undefined,
        },
        // The indicator is decorative fill; the progressbar ancestor owns the
        // value semantics, so the fill is hidden from assistive tech.
        indicator: { 'aria-hidden': 'true' },
      };
    },
    keymap: () => null,
  };

function parseVariant(raw: string | undefined): ProgressVariant {
  if (raw && (ALLOWED_VARIANTS as ReadonlyArray<string>).includes(raw)) {
    return raw as ProgressVariant;
  }
  return 'default';
}

function parseSize(raw: string | undefined): ProgressSize {
  if (raw && (ALLOWED_SIZES as ReadonlyArray<string>).includes(raw)) {
    return raw as ProgressSize;
  }
  return 'default';
}

function parseNumber(raw: string | undefined): number | undefined {
  if (raw === undefined) return undefined;
  const n = Number(raw);
  return Number.isFinite(n) ? n : undefined;
}

/**
 * Reconstruct the score's config from a root element's `data-*` attributes --
 * the inverse of the SSR/WC markup. Shared by bindProgress and the Web
 * Component so the two never drift on how an attribute maps to config.
 *
 * Config travels as `data-*` and nothing else (#2001). `value`/`max` are real
 * attributes on <progress>, not on the <div> this root actually is, and `size`
 * is real on input/select -- borrowing them here was invalid markup that never
 * reached `dataset`. `aria-label` is NOT read here: it is a native passthrough
 * on the progressbar (host === root), left untouched by the projection.
 */
export function readProgressConfig(root: HTMLElement): ProgressConfig {
  const data = root.dataset;
  return {
    value: parseNumber(data['value']),
    max: parseNumber(data['max']),
    valueText: data['valueText'],
    variant: parseVariant(data['variant']),
    size: parseSize(data['size']),
  };
}

/**
 * The DOM-native binding of the progress score -- the client the Web Component
 * and the Astro <script> both import. Progress is a STATIC score with no
 * effects, so the binding is the thinnest of the family: it re-reads config
 * from the root attributes, applies the resolved ARIA projection to the root
 * (the progressbar) and the indicator, and sets the indicator's inline fill
 * width (a percentage -- not expressible as a class). Only React reads the
 * projection declaratively.
 *
 * Three-gotcha ledger:
 *   1. Controlled-callback before/after: N/A. Progress has no actions and no
 *      internal state; `value` is config, so there is nothing to compare.
 *   2. aria-manager coerces the resolved string 'false' to truthy -- the
 *      projection is already final, so apply it with { validate: false }.
 *   3. WC bind deferred one microtask -- see progress.element.ts.
 *
 * `root` IS the progressbar (host === root in the WC; the track div in
 * React/Astro), so the projection lands on the element the binding is handed;
 * the indicator is its `[data-part="indicator"]` child.
 */
export function bindProgress(root: HTMLElement): () => void {
  const config = readProgressConfig(root);

  const getPart = (part: string): HTMLElement | null =>
    part === 'root' ? root : root.querySelector<HTMLElement>(`[data-part="${part}"]`);

  const { memory } = createBehavior(progress, config);

  // The projection is already resolved (final strings, undefined = absent), so
  // apply it raw: validate:false skips aria-manager's author-input coercion,
  // which would re-read a string like 'false' as truthy.
  const applyProjection = (el: HTMLElement, attrs: AriaAttrs) => {
    for (const [name, value] of Object.entries(attrs)) {
      updateAriaAttribute(el, name as never, value as never, { validate: false });
    }
  };

  const render = () => {
    const state = memory.get();
    const projection = progress.aria(state, config, { root: root.id ?? '', indicator: '' });
    if (projection.root) applyProjection(root, projection.root);

    const indicator = getPart('indicator');
    if (indicator) {
      if (projection.indicator) applyProjection(indicator, projection.indicator);
      const { indeterminate, percent } = resolveProgress(config);
      if (indeterminate) {
        indicator.style.removeProperty('width');
      } else {
        indicator.style.width = `${percent}%`;
      }
    }
  };
  const unsubscribe = memory.subscribe(render); // fires immediately: first paint

  return () => {
    unsubscribe();
  };
}
