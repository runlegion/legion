import type { EmptyConfig, EmptyState } from '@/components/ui/empty.behavior';

export interface EmptyClassSet {
  root: string;
}

/**
 * The placeholder's structure: a centered column that owns its own vertical
 * breathing room. Ported verbatim from the oracle
 * -- a flex column, centered on both axes, with a gap between regions, generous
 * block padding, and centered text.
 */
const emptyStructureClasses = 'flex flex-col items-center justify-center gap-4 py-12 text-center';

/**
 * Sub-part classes are config-independent literals, so the framework files
 * import them directly (no context/provider needed for a flat static). Ported
 * verbatim from the oracle; `text-title-medium ts-title-medium` / `text-body-small ts-body-small` are the
 * semantic typography role tokens, and the muted-foreground pairing keeps the
 * placeholder supportive rather than alarming.
 */
export const emptyIconClasses = 'text-muted-foreground [&>svg]:h-12 [&>svg]:w-12';

export const emptyTitleClasses = 'text-title-medium ts-title-medium text-foreground';

export const emptyDescriptionClasses =
  'max-w-sm text-body-small ts-body-small text-muted-foreground';

/** The action row carries no layout of its own -- the parent column centers it. */
export const emptyActionClasses = '';

export function emptyClasses(_config: EmptyConfig, _state: EmptyState): EmptyClassSet {
  return { root: emptyStructureClasses };
}
