import type {
  ActionPayloads,
  AriaAttrs,
  BehaviorSpec,
  KeyInput,
  PartDecl,
  PartIds,
} from '@/lib/contract';

/** Order-insensitive structural serialization: two PartDecls with the same
 *  entries in a different key order are the SAME declaration, not a collision.
 *  Plain `JSON.stringify` is key-order sensitive and would throw a false
 *  "declared twice with different PartDecls" on identical decls. */
function canonicalPartDecl(decl: unknown): string {
  return JSON.stringify(decl, (_key, value) =>
    value && typeof value === 'object' && !Array.isArray(value)
      ? Object.fromEntries(
          Object.entries(value as Record<string, unknown>).sort(([a], [b]) =>
            a < b ? -1 : a > b ? 1 : 0,
          ),
        )
      : value,
  );
}

export interface Slice<Config, State, Actions extends ActionPayloads, Part extends string> {
  name: string;
  parts?: Partial<Record<Part, PartDecl>>;
  initialState: (config: Config) => State;
  actions?: {
    [K in keyof Actions]: (state: State, payload: Actions[K]) => State;
  };
  canDispatch?: (state: State, action: keyof Actions, config: Config) => boolean;
  aria?: (state: State, config: Config, ids: PartIds<Part>) => Partial<Record<Part, AriaAttrs>>;
  keymap?: (event: KeyInput, state: State, part: Part, config: Config) => keyof Actions | null;
}

export interface GlueSlice<
  Config,
  MergedState,
  Actions extends ActionPayloads,
  Part extends string,
> {
  kind: 'glue';
  name: string;
  actions?: {
    [K in keyof Actions]: (state: MergedState, payload: Actions[K]) => MergedState;
  };
  canDispatch?: (state: MergedState, action: keyof Actions, config: Config) => boolean;
  aria?: (
    state: MergedState,
    config: Config,
    ids: PartIds<Part>,
  ) => Partial<Record<Part, AriaAttrs>>;
  keymap?: (
    event: KeyInput,
    state: MergedState,
    part: Part,
    config: Config,
  ) => keyof Actions | null;
}

export type DisjointFrom<A, B> = [Extract<keyof A, keyof B>] extends [never]
  ? unknown
  : { __stateKeyCollision: Extract<keyof A, keyof B> };

type UnknownState = Record<string, unknown>;
type UnknownSlice = Slice<unknown, UnknownState, ActionPayloads, string>;
type UnknownGlue = GlueSlice<unknown, UnknownState, ActionPayloads, string>;

function isGlue(candidate: UnknownSlice | UnknownGlue): candidate is UnknownGlue {
  return 'kind' in candidate && candidate.kind === 'glue';
}

export function compose<Config, S1 extends object, A1 extends ActionPayloads, P1 extends string>(
  name: string,
  slice: Slice<Config, S1, A1, P1>,
): BehaviorSpec<Config, S1, A1, P1>;
export function compose<
  Config,
  S1 extends object,
  A1 extends ActionPayloads,
  P1 extends string,
  GA extends ActionPayloads,
>(
  name: string,
  slice: Slice<Config, S1, A1, P1>,
  glue: GlueSlice<Config, S1, GA, P1>,
): BehaviorSpec<Config, S1, A1 & GA, P1>;
export function compose<
  Config,
  S1 extends object,
  A1 extends ActionPayloads,
  P1 extends string,
  S2 extends object,
  A2 extends ActionPayloads,
  P2 extends string,
>(
  name: string,
  first: Slice<Config, S1, A1, P1>,
  second: Slice<Config, S2, A2, P2> & DisjointFrom<S2, S1>,
): BehaviorSpec<Config, S1 & S2, A1 & A2, P1 | P2>;
export function compose<
  Config,
  S1 extends object,
  A1 extends ActionPayloads,
  P1 extends string,
  S2 extends object,
  A2 extends ActionPayloads,
  P2 extends string,
  GA extends ActionPayloads,
>(
  name: string,
  first: Slice<Config, S1, A1, P1>,
  second: Slice<Config, S2, A2, P2> & DisjointFrom<S2, S1>,
  glue: GlueSlice<Config, S1 & S2, GA, P1 | P2>,
): BehaviorSpec<Config, S1 & S2, A1 & A2 & GA, P1 | P2>;
export function compose(
  name: string,
  ...rawEntries: ReadonlyArray<object>
): BehaviorSpec<unknown, UnknownState, ActionPayloads, string> {
  const entries = rawEntries as ReadonlyArray<UnknownSlice | UnknownGlue>;
  const glue = entries.filter(isGlue);
  if (glue.length > 1) {
    throw new Error(`compose(${name}): at most one glue slice (got ${glue.length})`);
  }
  if (glue.length === 1 && !isGlue(entries[entries.length - 1] as UnknownSlice | UnknownGlue)) {
    throw new Error(`compose(${name}): the glue slice must be last in the fold`);
  }
  const slices = entries.filter((e): e is UnknownSlice => !isGlue(e));
  if (slices.length === 0) {
    throw new Error(`compose(${name}): at least one non-glue slice is required`);
  }
  const theGlue = glue[0];
  const contributors: ReadonlyArray<UnknownSlice | UnknownGlue> = entries;

  const parts: Record<string, PartDecl> = {};
  for (const slice of slices) {
    for (const [part, decl] of Object.entries(slice.parts ?? {})) {
      const existing = parts[part];
      if (existing && canonicalPartDecl(existing) !== canonicalPartDecl(decl)) {
        throw new Error(`compose(${name}): part "${part}" declared twice with different PartDecls`);
      }
      parts[part] = decl as PartDecl;
    }
  }

  const actionOwners = new Map<string, string>();
  for (const entry of contributors) {
    for (const action of Object.keys(entry.actions ?? {})) {
      const owner = actionOwners.get(action);
      if (owner) {
        throw new Error(
          `compose(${name}): action "${action}" defined by both "${owner}" and "${entry.name}"`,
        );
      }
      actionOwners.set(action, entry.name);
    }
  }
  const actions: Record<string, (state: UnknownState, payload: unknown) => UnknownState> = {};
  for (const entry of contributors) {
    Object.assign(actions, entry.actions ?? {});
  }

  return {
    name,
    parts,
    initialState: (config) => {
      const merged: UnknownState = {};
      for (const slice of slices) {
        const contribution = slice.initialState(config);
        for (const key of Object.keys(contribution)) {
          if (key in merged) {
            throw new Error(
              `compose(${name}): state key "${key}" contributed by more than one slice`,
            );
          }
        }
        Object.assign(merged, contribution);
      }
      return merged;
    },
    actions,
    canDispatch: (state, action, config) =>
      contributors.every((entry) => entry.canDispatch?.(state, action, config) ?? true),
    aria: (state, config, ids) => {
      const merged: Record<string, AriaAttrs> = {};
      for (const slice of slices) {
        const contribution = slice.aria?.(state, config, ids) ?? {};
        for (const [part, attrs] of Object.entries(contribution)) {
          const target = merged[part] ?? (merged[part] = {});
          for (const [attr, value] of Object.entries(attrs ?? {})) {
            if (attr in target) {
              throw new Error(
                `compose(${name}): aria attribute "${attr}" on part "${part}" set by two slices -- resolve in the glue slice`,
              );
            }
            target[attr] = value;
          }
        }
      }
      const glueContribution = theGlue?.aria?.(state, config, ids) ?? {};
      for (const [part, attrs] of Object.entries(glueContribution)) {
        merged[part] = { ...merged[part], ...attrs };
      }
      return merged;
    },
    keymap: (event, state, part, config) => {
      const glueClaim = theGlue?.keymap?.(event, state, part, config) ?? null;
      if (glueClaim !== null) return glueClaim;
      const claims = slices
        .map((slice) => ({ slice, action: slice.keymap?.(event, state, part, config) ?? null }))
        .filter((claim) => claim.action !== null);
      if (claims.length > 1) {
        throw new Error(
          `compose(${name}): key "${event.key}" on part "${part}" claimed by ${claims
            .map((c) => `"${c.slice.name}"`)
            .join(' and ')} -- resolve in the glue slice`,
        );
      }
      return claims[0]?.action ?? null;
    },
  };
}
