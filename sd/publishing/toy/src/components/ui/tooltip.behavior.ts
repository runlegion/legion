import { compose, type GlueSlice } from '@/lib/compose';
import {
  createBehavior,
  type AriaAttrs,
  type BehaviorSpec,
  type PartIds,
} from '@/lib/contract';
import {
  disclosable,
  isOpen,
  type DisclosableActions,
  type DisclosableConfig,
  type DisclosablePart,
  type DisclosableState,
} from '@/lib/disclosable';
import { computePosition } from '@/lib/primitives/collision-detector';
import { updateAriaAttribute } from '@/lib/primitives/aria-manager';
import type { Align, Side } from '@/lib/primitives/types';

export interface TooltipConfig extends DisclosableConfig {
  /** When true, moving the pointer onto the content does NOT hold it open.
   *  Default false (content is hoverable). Reflected as
   *  `data-disable-hoverable-content` and read by the CSS reveal rule -- the
   *  hover-intent delay itself is `transition-delay`, never a timer. */
  disableHoverableContent?: boolean | undefined;
  /** Preferred side of the anchor to float the content. Default 'top'. */
  side?: Side | undefined;
  /** Alignment along the chosen side. Default 'center'. */
  align?: Align | undefined;
  /** Gap between anchor and content, px. Default 4. */
  sideOffset?: number | undefined;
}

export type TooltipState = DisclosableState;
export type TooltipActions = DisclosableActions;
export type TooltipPart = DisclosablePart;

export { isOpen };

/** Resolved placement, defaults applied. Read by both clients and the shared
 *  positioner so the placement decision lives in ONE place, never a decorator. */
export function tooltipPlacement(config: TooltipConfig): {
  side: Side;
  align: Align;
  sideOffset: number;
} {
  return {
    side: config.side ?? 'top',
    align: config.align ?? 'center',
    sideOffset: config.sideOffset ?? 4,
  };
}

/**
 * The tooltip glue: the tooltip ARIA identity (aria-describedby, role=tooltip)
 * and the Escape dismiss contract, written over the disclosable open axis.
 *
 * A tooltip is NOT a disclosure widget -- it describes, it does not expand --
 * so the disclosable trigger projection (aria-expanded / aria-controls) is
 * suppressed here and replaced with aria-describedby, the tooltip's real wiring.
 */
const tooltipGlue: GlueSlice<TooltipConfig, TooltipState, { close: undefined }, TooltipPart> = {
  kind: 'glue',
  name: 'tooltip',
  aria: (_state, _config, ids) => {
    return {
      trigger: {
        // Suppress the disclosure projection: a tooltip trigger is described,
        // not expanded. Projected undefined => the attribute is not rendered.
        'aria-expanded': undefined,
        'aria-controls': undefined,
        // UNCONDITIONAL (#2148). The content is present in the DOM at all times
        // and reveal is a CSS concern now, so gating the link on `open` would
        // mean the description only exists during the frames JavaScript happens
        // to consider the tip open -- and on a JS-off page, never.
        'aria-describedby': ids.content ? ids.content : undefined,
      },
      content: {
        role: 'tooltip',
      },
    };
  },
  // WAI-ARIA tooltip pattern: Escape dismisses the tip. The idempotence gate
  // makes this a no-op when already closed.
  // Positioning is a DOM concern composed directly by the clients
  // (collision-detector), not behavior state; hover-intent TIMING is CSS.
  keymap: (event) => (event.key === 'Escape' ? 'close' : null),
};

export const tooltip: BehaviorSpec<TooltipConfig, TooltipState, TooltipActions, TooltipPart> =
  compose('tooltip', disclosable<TooltipConfig>(), tooltipGlue);

/** Apply the resolved aria projection to an element (validate:false skips the
 *  author-input coercion that would flip a projected 'false'). */
function applyProjection(el: HTMLElement, attrs: AriaAttrs): void {
  for (const [name, value] of Object.entries(attrs)) {
    updateAriaAttribute(el, name as never, value as never, { validate: false });
  }
}

/**
 * Float the content beside its anchor via the collision-detector primitive and
 * stamp the resolved side/align onto the content for motion hooks. Shared by
 * every client so the positioning composition is written exactly once.
 */
export function positionTooltipContent(
  trigger: HTMLElement,
  content: HTMLElement,
  config: TooltipConfig,
): void {
  const placement = tooltipPlacement(config);
  const result = computePosition(trigger, content, {
    side: placement.side,
    align: placement.align,
    sideOffset: placement.sideOffset,
    avoidCollisions: true,
  });
  content.style.position = 'fixed';
  content.style.left = '0';
  content.style.top = '0';
  content.style.transform = `translate(${Math.round(result.x)}px, ${Math.round(result.y)}px)`;
  content.dataset['side'] = result.side;
  content.dataset['align'] = result.align;
}

/**
 * The DOM-native binding of the tooltip score -- the client the Web Component
 * and the Astro <script> both import. Only React reads the projections
 * declaratively.
 *
 * MOTION IS NOT HERE (#2148). The hover-intent delay is `transition-delay` in
 * tooltip.classes.ts and the reveal is native `:hover` / `:focus-visible`, so
 * this binding contains no timer and reads no motion token. What it still does
 * is the JS-ON ENHANCEMENT: track the open axis so `data-state`,
 * `onOpenChange`, and the collision-detector's positioning follow real pointer
 * and focus interaction, and honour the WCAG 1.4.13 Escape dismissal, which CSS
 * alone cannot express. Every dispatch below is immediate -- there is no
 * timer anywhere in this file.
 *
 * The content is present in the DOM unconditionally and NEVER carries `hidden`:
 * `hidden` is UA-stylesheet `display: none`, which pulls the node out of the
 * accessibility tree (breaking the unconditional aria-describedby) and out of
 * rendering (killing the transition and the `:hover` reveal alike).
 */
export function bindTooltip(root: HTMLElement): () => void {
  const getPart = (part: string): HTMLElement | null =>
    part === 'root' ? root : root.querySelector<HTMLElement>(`[data-part="${part}"]`);

  // Config travels as `data-*` and nothing else (#2001/#2004), so the read is
  // `dataset` by camelCase key. No delay attributes remain to parse: the two
  // that used to live here (`data-delay-duration` / `data-skip-delay-duration`)
  // were the JS half of a timing decision that is now entirely CSS.
  const data = root.dataset;
  const numData = (key: string, fallback: number): number => {
    const raw = data[key];
    if (raw === undefined || raw === '') return fallback;
    const parsed = Number(raw);
    return Number.isFinite(parsed) ? parsed : fallback;
  };

  const content = getPart('content');
  const config: TooltipConfig = {
    disableHoverableContent: data['disableHoverableContent'] === 'true',
    defaultOpen: data['defaultOpen'] === 'true' || content?.dataset['state'] === 'open',
    side: (data['side'] as Side | undefined) ?? undefined,
    align: (data['align'] as Align | undefined) ?? undefined,
    // Non-empty presence, not truthiness: `data-side-offset="0"` is a real 0px
    // offset, while a blank `data-side-offset=""` is absence and takes the 4px
    // default -- `numData` rejects the empty string, so both the blank and the
    // wholly absent attribute land on the same 4.
    sideOffset: 'sideOffset' in data ? numData('sideOffset', 4) : undefined,
  };

  const { memory, dispatch } = createBehavior(tooltip, config);
  const request = (action: keyof TooltipActions): boolean => dispatch(action, config);

  // ids READ from the server/author markup, never generated.
  const ids = {} as PartIds<TooltipPart>;
  for (const part of Object.keys(tooltip.parts) as TooltipPart[])
    ids[part] = getPart(part)?.id ?? '';

  const trigger = getPart('trigger');

  const render = () => {
    const state = memory.get();
    const open = isOpen(state, config);
    const projection = tooltip.aria(state, config, ids);
    for (const part of Object.keys(projection) as TooltipPart[]) {
      const attrs = projection[part];
      const el = getPart(part);
      if (el && attrs) applyProjection(el, attrs);
    }
    if (content) {
      // The SSR markup stamps data-state once; keep it in step afterwards so
      // the `data-[state=open]` reveal path tracks a controlled/forced open.
      content.dataset['state'] = open ? 'open' : 'closed';
      if (open && trigger) positionTooltipContent(trigger, content, config);
    }
  };
  const unsubscribe = memory.subscribe(render); // fires immediately: first paint

  const reposition = () => {
    if (content && trigger && isOpen(memory.get(), config)) {
      positionTooltipContent(trigger, content, config);
    }
  };

  // The dismissal flag the CSS force-hides on (WCAG 1.4.13 "dismissible"): a
  // hovered tip that Escape dismissed must stay gone until the pointer leaves,
  // and `:hover` alone cannot remember that a dismissal happened.
  const clearDismissed = () => {
    delete root.dataset['dismissed'];
  };

  const onKeydown = (event: KeyboardEvent) => {
    const partEl = (event.target as HTMLElement).closest<HTMLElement>('[data-part]');
    const part = partEl?.dataset['part'] as TooltipPart | undefined;
    if (!part) return;
    const action = tooltip.keymap(
      {
        key: event.key,
        shiftKey: event.shiftKey,
        ctrlKey: event.ctrlKey,
        altKey: event.altKey,
        metaKey: event.metaKey,
      },
      memory.get(),
      part,
      config,
    );
    if (action !== 'close') return;
    event.preventDefault();
    request('close');
    root.dataset['dismissed'] = 'true';
  };

  // The hover scope mirrors the CSS reveal rule exactly: the root by default
  // (so the pointer can travel onto the tip), the trigger alone when the
  // content is declared un-hoverable. These dispatches move `data-state`,
  // `onOpenChange`, and positioning -- visibility is the stylesheet's, always.
  const hoverScope = config.disableHoverableContent ? trigger : root;

  // A standing dismissal may only be dropped once NOTHING can still reveal the
  // tip. The reveal rule has two independent halves -- the hover scope's
  // `:hover` and the trigger's `:focus-visible` -- so clearing on one axis while
  // the other is still matching brings the dismissed tip straight back at
  // opacity 1 against `data-state="closed"` (WCAG 1.4.13). Each leave handler
  // therefore checks the OTHER axis, and whichever leaves last does the clear.
  const triggerFocused = () => trigger !== null && trigger === document.activeElement;
  const scopeHovered = () => hoverScope?.matches(':hover') === true;

  const onPointerEnter = () => void request('open');
  const onPointerLeave = () => {
    if (!triggerFocused()) clearDismissed();
    request('close');
  };
  const onFocus = () => void request('open');
  const onBlur = () => {
    if (!scopeHovered()) clearDismissed();
    request('close');
  };

  hoverScope?.addEventListener('pointerenter', onPointerEnter);
  hoverScope?.addEventListener('pointerleave', onPointerLeave);
  trigger?.addEventListener('focus', onFocus);
  trigger?.addEventListener('blur', onBlur);
  root.addEventListener('keydown', onKeydown);
  window.addEventListener('scroll', reposition, { capture: true, passive: true });
  window.addEventListener('resize', reposition, { passive: true });

  return () => {
    unsubscribe();
    hoverScope?.removeEventListener('pointerenter', onPointerEnter);
    hoverScope?.removeEventListener('pointerleave', onPointerLeave);
    trigger?.removeEventListener('focus', onFocus);
    trigger?.removeEventListener('blur', onBlur);
    root.removeEventListener('keydown', onKeydown);
    window.removeEventListener('scroll', reposition, { capture: true });
    window.removeEventListener('resize', reposition);
  };
}
