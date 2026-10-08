/**
 * EmotionCurve: legion component.
 *
 * Why not a composite: the curve is a line through two points per phase
 * (`emotional_start`, `emotional_end`) on a fixed -3..+3 scale. Rafters'
 * `chart` ships only `ChartContainer` (config + measured plot region); the
 * Line/Area marks it refers to are not installed, and no block can turn a
 * bound array into SVG coordinates. The container is still rafters': this
 * component draws inside it and takes its series colour from the chart
 * token config.
 */
import { z } from "zod";
import { ChartContainer } from "@/components/ui/chart";

const PhaseSchema = z.object({
  number: z.number(),
  title: z.string(),
  emotional_start: z.number(),
  emotional_end: z.number(),
});

const PhasesSchema = z.array(PhaseSchema);

const COLUMN = 240;
const TOP = 24;
const ROW = 28;
const LEFT = 48;
const SCALE = [3, 2, 1, 0, -1, -2, -3];

function y(score: number): number {
  return TOP + (3 - score) * ROW;
}

export function EmotionCurve({ phases }: { phases?: unknown }) {
  const parsed = PhasesSchema.safeParse(phases);
  if (!parsed.success) return null;
  const list = parsed.data;
  const width = LEFT + list.length * COLUMN;
  const height = TOP + 6 * ROW + 56;

  const points: { x: number; y: number; score: number; key: string }[] = [];
  for (const [index, phase] of list.entries()) {
    const x0 = LEFT + index * COLUMN;
    points.push({ x: x0 + COLUMN * 0.2, y: y(phase.emotional_start), score: phase.emotional_start, key: `${phase.number}s` });
    points.push({ x: x0 + COLUMN * 0.8, y: y(phase.emotional_end), score: phase.emotional_end, key: `${phase.number}e` });
  }
  const path = points.map((p, i) => `${i === 0 ? "M" : "L"}${p.x},${p.y}`).join(" ");

  return (
    <ChartContainer config={{ emotion: { label: "Emotion", token: "chart-1" } }}>
      <figure aria-label="Emotion curve across the journey's phases">
        <svg viewBox={`0 0 ${width} ${height}`} className="w-full h-auto" role="img" aria-hidden="true">
          {SCALE.map((score) => (
            <g key={score}>
              <line
                x1={LEFT}
                x2={width}
                y1={y(score)}
                y2={y(score)}
                className={score === 0 ? "stroke-border" : "stroke-muted"}
                strokeWidth={score === 0 ? 1.5 : 1}
                strokeDasharray={score === 0 ? undefined : "2 4"}
              />
              <text x={LEFT - 12} y={y(score) + 4} textAnchor="end" className="fill-muted-foreground text-label-small">
                {score > 0 ? `+${score}` : score}
              </text>
            </g>
          ))}
          {list.map((phase, index) => (
            <g key={phase.number}>
              {index > 0 ? (
                <line
                  x1={LEFT + index * COLUMN}
                  x2={LEFT + index * COLUMN}
                  y1={TOP - 8}
                  y2={y(-3) + 8}
                  className="stroke-border"
                  strokeWidth={1}
                />
              ) : null}
              <text
                x={LEFT + index * COLUMN + COLUMN / 2}
                y={y(-3) + 36}
                textAnchor="middle"
                className="fill-foreground text-label-medium"
              >
                {phase.number}. {phase.title}
              </text>
            </g>
          ))}
          <path d={path} fill="none" className="stroke-chart-1" strokeWidth={2.5} strokeLinejoin="round" />
          {points.map((p) => (
            <circle key={p.key} cx={p.x} cy={p.y} r={5} className="fill-chart-1 stroke-card" strokeWidth={2}>
              <title>{p.score}</title>
            </circle>
          ))}
        </svg>
        <figcaption className="text-label-small text-muted-foreground">
          Each phase plots its emotional start and end on a -3 to +3 scale.
        </figcaption>
      </figure>
    </ChartContainer>
  );
}
