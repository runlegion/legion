/**
 * EcosystemRings: legion component.
 *
 * Why not a composite: the map places each actor on a ring by its tier
 * (`actors.primary/secondary/tertiary`) at a computed angle, and draws each
 * `value_exchanges[]` entry as a curve between two placed actors, coloured by
 * `kind`. Placement is geometry over the whole array (angle = index / count),
 * and a flow needs a lookup from a name in `from`/`to` to a placed point.
 * Neither is a bind, and rafters ships no diagram, graph-layout or chart
 * mark that takes nodes and edges.
 */
import { z } from "zod";
import { ChartContainer } from "@/components/ui/chart";

const ActorSchema = z.object({ name: z.string(), role: z.string().optional() });
const ActorsSchema = z.object({
  primary: z.array(ActorSchema).default([]),
  secondary: z.array(ActorSchema).default([]),
  tertiary: z.array(ActorSchema).default([]),
});
const KindSchema = z.enum(["person", "product", "money", "knowledge", "trust"]);
const ExchangeSchema = z.object({ from: z.string(), to: z.string(), kind: KindSchema, gives: z.string().optional() });

type Kind = z.infer<typeof KindSchema>;

/** Fixed order: colour follows the kind, never its rank or count. */
const KINDS: readonly { kind: Kind; stroke: string; fill: string }[] = [
  { kind: "person", stroke: "stroke-chart-1", fill: "fill-chart-1" },
  { kind: "product", stroke: "stroke-chart-2", fill: "fill-chart-2" },
  { kind: "money", stroke: "stroke-chart-3", fill: "fill-chart-3" },
  { kind: "knowledge", stroke: "stroke-chart-4", fill: "fill-chart-4" },
  { kind: "trust", stroke: "stroke-chart-5", fill: "fill-chart-5" },
];

const SIZE = 960;
const C = SIZE / 2;
const RINGS = [
  { tier: "primary", radius: 200, start: -Math.PI / 2 },
  { tier: "secondary", radius: 320, start: -Math.PI / 2 + 0.3 },
  { tier: "tertiary", radius: 430, start: -Math.PI / 2 + 0.15 },
] as const;

interface Point {
  x: number;
  y: number;
}

function inset(from: Point, toward: Point): Point {
  const r = from.x === C && from.y === C ? 70 : 10;
  const dx = toward.x - from.x;
  const dy = toward.y - from.y;
  const len = Math.hypot(dx, dy) || 1;
  return { x: from.x + (dx / len) * r, y: from.y + (dy / len) * r };
}

function wrap(text: string, width: number): string[] {
  const lines: string[] = [];
  let line = "";
  for (const word of text.split(" ")) {
    if (line.length > 0 && line.length + word.length + 1 > width) {
      lines.push(line);
      line = word;
    } else {
      line = line.length > 0 ? `${line} ${word}` : word;
    }
  }
  if (line.length > 0) lines.push(line);
  return lines.slice(0, 3);
}

export function EcosystemRings({
  core,
  actors,
  exchanges,
}: {
  core?: unknown;
  actors?: unknown;
  exchanges?: unknown;
}) {
  const tiers = ActorsSchema.safeParse(actors);
  const flows = z.array(ExchangeSchema).safeParse(exchanges);
  if (!tiers.success || !flows.success) {
    return <pre className="text-destructive">{tiers.error?.message ?? flows.error?.message}</pre>;
  }
  const coreName = typeof core === "string" ? core : "legion";

  const placed = new Map<string, Point>();
  placed.set(coreName, { x: C, y: C });
  const nodes: { name: string; tier: string; at: Point }[] = [];
  for (const ring of RINGS) {
    const list = tiers.data[ring.tier];
    for (const [index, actor] of list.entries()) {
      const angle = ring.start + (index / Math.max(list.length, 1)) * Math.PI * 2;
      const at = { x: C + ring.radius * Math.cos(angle), y: C + ring.radius * Math.sin(angle) };
      placed.set(actor.name, at);
      nodes.push({ name: actor.name, tier: ring.tier, at });
    }
  }

  // An endpoint that names two actors joined by " / " fans out to each.
  const resolve = (name: string): Point[] => {
    const direct = placed.get(name);
    if (direct) return [direct];
    const parts: Point[] = [];
    for (const part of name.split(" / ")) {
      const found = placed.get(part.trim());
      if (found) parts.push(found);
    }
    return parts;
  };

  const counts = new Map<Kind, number>();
  const unplaced = new Set<string>();
  const edges: { key: string; d: string; kind: Kind }[] = [];
  for (const [index, flow] of flows.data.entries()) {
    counts.set(flow.kind, (counts.get(flow.kind) ?? 0) + 1);
    const froms = resolve(flow.from);
    const tos = resolve(flow.to);
    if (froms.length === 0) unplaced.add(flow.from);
    if (tos.length === 0) unplaced.add(flow.to);
    const kindIndex = KINDS.findIndex((k) => k.kind === flow.kind);
    for (const rawA of froms) {
      for (const rawB of tos) {
        // Stop each curve at the edge of its node so arrowheads stay visible.
        const a = inset(rawA, rawB);
        const b = inset(rawB, rawA);
        const mx = (a.x + b.x) / 2;
        const my = (a.y + b.y) / 2;
        const dx = b.x - a.x;
        const dy = b.y - a.y;
        const len = Math.hypot(dx, dy) || 1;
        const bend = 28 + kindIndex * 14;
        const cx = mx + (-dy / len) * bend;
        const cy = my + (dx / len) * bend;
        edges.push({ key: `${index}-${a.x}-${b.x}`, d: `M${a.x},${a.y} Q${cx},${cy} ${b.x},${b.y}`, kind: flow.kind });
      }
    }
  }

  return (
    <ChartContainer
      config={{
        person: { label: "person", token: "chart-1" },
        product: { label: "product", token: "chart-2" },
        money: { label: "money", token: "chart-3" },
        knowledge: { label: "knowledge", token: "chart-4" },
        trust: { label: "trust", token: "chart-5" },
      }}
    >
      <figure aria-label="Ecosystem map: actors in rings by tier and the value flowing between them">
        <ul className="flex flex-wrap gap-4 text-label-medium text-foreground" aria-label="Flow kinds">
          {KINDS.map((k) => (
            <li key={k.kind} className="flex items-center gap-2">
              <svg width="28" height="10" aria-hidden="true">
                <line x1="0" x2="28" y1="5" y2="5" className={k.stroke} strokeWidth={3} />
              </svg>
              {k.kind} ({counts.get(k.kind) ?? 0})
            </li>
          ))}
        </ul>
        <svg viewBox={`0 0 ${SIZE} ${SIZE}`} className="w-full h-auto max-w-5xl" aria-hidden="true">
          <defs>
            {KINDS.map((k) => (
              <marker key={k.kind} id={`arrow-${k.kind}`} viewBox="0 0 10 10" refX="9" refY="5" markerWidth="6" markerHeight="6" orient="auto-start-reverse">
                <path d="M0,0 L10,5 L0,10 z" className={k.fill} />
              </marker>
            ))}
          </defs>
          {RINGS.map((ring) => (
            <g key={ring.tier}>
              <circle cx={C} cy={C} r={ring.radius} fill="none" className="stroke-border" strokeWidth={1} strokeDasharray="3 5" />
              <text x={C} y={C - ring.radius - 8} textAnchor="middle" className="fill-muted-foreground text-label-small uppercase">
                {ring.tier}
              </text>
            </g>
          ))}
          {edges.map((edge) => {
            const style = KINDS.find((k) => k.kind === edge.kind);
            return (
              <path
                key={edge.key}
                d={edge.d}
                fill="none"
                className={style?.stroke}
                strokeWidth={1.75}
                strokeOpacity={0.75}
                markerEnd={`url(#arrow-${edge.kind})`}
              />
            );
          })}
          <circle cx={C} cy={C} r={64} className="fill-primary" />
          <text x={C} y={C + 6} textAnchor="middle" className="fill-primary-foreground text-title-medium">
            {coreName}
          </text>
          {nodes.map((node) => {
            const lines = wrap(node.name, 22);
            return (
              <g key={node.name}>
                <circle cx={node.at.x} cy={node.at.y} r={7} className="fill-card stroke-foreground" strokeWidth={2} />
                {lines.map((line, i) => (
                  <text
                    key={line}
                    x={node.at.x}
                    y={node.at.y + 22 + i * 14}
                    textAnchor="middle"
                    className="fill-foreground stroke-background text-label-small"
                    paintOrder="stroke"
                    strokeWidth={4}
                  >
                    {line}
                  </text>
                ))}
              </g>
            );
          })}
        </svg>
        {unplaced.size > 0 ? (
          <figcaption className="text-label-small text-muted-foreground">
            Flows with an endpoint not among the actors: {[...unplaced].join("; ")}
          </figcaption>
        ) : null}
      </figure>
    </ChartContainer>
  );
}
