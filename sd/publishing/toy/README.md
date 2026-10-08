# Publishing toy: six documents through rafters composites

A toy for learning what rafters composites can and cannot do with the workshop's documents. It isn't production code.
It renders the six legion 0.5 documents in `data/` (exported with `legion document view <id> --json`, `.payload`)
through one `.composite.json` per document type, as the contract in `../README.md` describes.

```
pnpm install
pnpm build         # tsc --noEmit, then vite build into dist/
pnpm screenshots   # serves dist/ with vite preview, writes screenshots/*.png with Playwright
```

## How it renders

1. `src/composites/*.composite.json` holds one composite per document type: intent-sheet, question-board, persona-card,
   journey-map, service-blueprint and ecosystem-map. Each one is a manifest, I/O rules and a flat block tree in rafters' format,
   validated at load with rafters' own `CompositeFileSchema`.
2. `src/bind/expand.ts` is the binding pass. It takes a composite and a payload, resolves every `$bind`, unrolls every
   `repeat`, and returns plain `CompositeBlock[]`.
3. Rafters' own `Composite` / `toJsx` (`src/composites/runtime/`, installed by `rafters add composites`) renders those
   blocks against `src/render/components.tsx`, a map of block types to installed rafters components plus five legion
   components. A block type the map lacks renders as a visible "missing block type" marker, and the screenshot script
   fails if one appears.

## What is a composite

All six pages are composites. Every heading, badge, card, alert, table, grid and list on them is a rafters
component placed by a block. Inside the composites:

- intent-sheet: all of it.
- question-board: status lanes (repeat with `where` on `status`), question cards, few-voice marker, persona group table
  with story counts (`$count`), and the full citation register as a table with source links.
- persona-card: all of it except the inline citation links.
- journey-map: the phase-by-phase grid (a table whose columns are a repeat over `phases`, one row per lane), the
  moments of truth, the critical path and the open questions. The curve is a legion component.
- service-blueprint: header, actors, channels, moments of truth, metrics by step and evidence by step (nested repeats).
  The layer grid is a legion component.
- ecosystem-map: actors by tier, channels, value exchanges (kind badge via `$map`), moments of truth, failure register
  and answers. The ring map is a legion component.

## Legion components (block types the composites reference)

| Block type | File | Why it could not be a composite |
|---|---|---|
| `citation-text`, `citation-chips` | `src/legion/citations.tsx` | Citations sit inside prose as `[C5, C11]` (persona, journey, blueprint, ecosystem) or `(C14)` (Discovery answers), not as structured ids beside the text. A block can bind a field but cannot split a string into text and links. Turning an id into its quote, author and source is also a join against the Discovery's `citations[]`, and `$bind` is one path into one payload. |
| `emotion-curve` | `src/legion/emotion-curve.tsx` | Rafters `chart` installs only `ChartContainer` (token config and a measured plot area). It has no line or area marks, and no block turns a bound array into coordinates. The curve is drawn inside rafters' `ChartContainer`, coloured with the `chart-1` token. |
| `blueprint-grid` | `src/legion/blueprint-grid.tsx` | A table with a repeat over steps draws the plain layer grid. The blueprint also needs one lane per actor: a step goes in the lane of `steps[].actor`, and an omitted actor means the customer actor. A step is a moment of truth when its `number` is in `moments_of_truth[].step`. The lines of interaction, visibility and internal interaction fall between specific rows. The first two are joins with a default, and rafters `table` takes no row-level emphasis prop and no className. |
| `ecosystem-rings` | `src/legion/ecosystem-rings.tsx` | Each actor goes on its tier's ring at an angle computed from its index and the ring's count. Each value exchange is drawn as a curve between two placed actors, coloured by kind. That is geometry over the whole array plus a name-to-point lookup, and rafters ships no node and edge diagram. |

## What rafters lacked

- **`$bind` exists only in the README.** The installed `toJsx` copies `meta` into props unchanged. A
  `{ "$bind": "props.x" }` reaches the component as a literal object, and nothing in `packages/composites/src` reads
  `$bind`. The binding pass here fills the gap in front of rafters' renderer.
- **No iteration.** Nothing repeats a subtree per array item, and `walkBlocks` renders each child once, bottom up,
  so it cannot re-scope a subtree. Every document here is arrays: questions, phases, steps, actors, exchanges, failure modes.
  The toy adds a `repeat` block (`each`, `as`, optional `where` equality filter, `$index` and `$n` in scope).
- **No value operators.** Status to badge variant needs `$map`. Story counts need `$count`. "12 authors across 5
  families" needs a template (`$tpl`). A family list needs `$join`. Empty sections need `$if` / `$unless`.
  Each of these is in `src/bind/expand.ts`, and each is a gap.
- **No joins.** A bind is one path into one payload. Citation ids to the Discovery register, blueprint steps to actors
  and moments of truth, and exchange endpoints to placed actors all need a lookup. Those are three of the four legion
  components.
- **No reusable sub-template that takes scope.** The question card is used by three lanes. The only way to share it
  is to point three `repeat` blocks at the same child id. That works in this pass but breaks the tree's
  one-parent assumption. `composite:<id>` embeds blocks but passes no data.
- **`rafters add composites` installs a broken runtime.** It wrote the files to `lib/composites/` (outside `src/`, and
  not the `compositesPath` that `init` wrote to config) with imports rewritten to `@/components/ui/manifest`,
  `@/components/ui/walk-blocks` and so on, which don't exist. `bridge.ts` and `registry.ts` import `@rafters/ui/...`,
  which a consumer doesn't have, and `to-mdx.ts` needs `escape-html`, which wasn't installed. The toy moved the runtime to
  `src/composites/runtime/`, pointed the imports at `./`, and dropped those three files. No default composites ship.
- **`Container as="article"` caps the width at prose.** Article mode adds `max-w-prose` after the size class, so a
  document page wider than a column of prose can't be an article and loses article typography. The pages use
  `as="div"` with the typography components instead.
- **Card header stretches a badge to full width**, because the header is a grid. Badges have to be wrapped in a `p`.
- **No diagram, timeline or line chart.** Those are the three industry-standard visuals in the set: the curve, the
  blueprint swimlanes and the ecosystem rings.
- **Palette:** in this theme `chart-1` and `chart-2` are both teal, so person and product flows would be hard to tell
  apart. No exchange here has kind `person`, so the map doesn't hit it yet.

## Schema gaps the toy hit (fixes belong in the schemas first, per the contract)

- **Citations are not structured on persona, journey, blueprint or ecosystem fields.** They sit inside the prose,
  and the brackets differ between documents. `citation-text` is the one place the toy reads prose. With a
  `citations: string[]` beside each field, it becomes `citation-chips`, a plain repeat.
- **`failure_modes[]` has no status.** "OPEN", "ANSWERED", "Designed:" and "OUT OF SCOPE" are prefixes inside
  `recovery`, so the register can't split open from answered without parsing prose. The page says so.
- **Persona "thin" marking lives in prose**, as "(1 author, thin)". It's shown verbatim and not marked.
- **One ecosystem exchange endpoint names two actors**: "The operator who keeps re-teaching the same agent / The
  fleet operator who became the go-between". The ring map fans it out to both, which is a small parse, and the flow
  should be two entries.
- **The Discovery's few-voice questions are exactly its open ones** (5 of 5). A separate few-voices lane left the Open
  lane empty, so the board shows three status lanes, with a few-voices marker on each card that has the flag.
