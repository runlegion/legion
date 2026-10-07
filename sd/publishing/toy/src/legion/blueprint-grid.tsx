/**
 * BlueprintGrid: legion component.
 *
 * Why not a composite: a rafters `table` with a `repeat` over steps draws the
 * plain layer grid, but three things in the standard blueprint need more than
 * a path bind:
 *  - one lane per actor: each step's customer action goes in the lane of
 *    `steps[].actor`, defaulting to the customer actor when omitted (a join
 *    between `steps[]` and `actors[]` with a default);
 *  - moments of truth: a step is marked when its `number` appears in
 *    `moments_of_truth[].step` (another join);
 *  - the lines of interaction, visibility and internal interaction are drawn
 *    between specific layer rows, and the table component takes no row-level
 *    border or emphasis prop (and no className).
 */
import type { ReactNode } from "react";
import { z } from "zod";
import { Badge } from "@/components/ui/badge";
import { CitationText } from "@/legion/citations";

const StepSchema = z.object({
  number: z.number(),
  title: z.string(),
  time: z.string().optional(),
  actor: z.string().optional(),
  emotional_score: z.number().optional(),
  emotional_label: z.string().optional(),
  layers: z.object({
    evidence: z.string().optional(),
    customer_actions: z.string().optional(),
    frontstage: z.string().optional(),
    backstage: z.string().optional(),
    support: z.string().optional(),
  }),
  frictions: z.array(z.string()).default([]),
  fail_points: z.array(z.string()).default([]),
});

const BlueprintSchema = z.object({
  actors: z.array(z.object({ name: z.string(), role: z.enum(["customer", "on_stage"]) })),
  moments_of_truth: z.array(z.object({ step: z.number(), title: z.string() })).default([]),
  steps: z.array(StepSchema),
});

type Step = z.infer<typeof StepSchema>;

function RowLabel({ children, hint }: { children: ReactNode; hint?: string }) {
  return (
    <div className="sticky left-0 z-10 bg-muted px-3 py-2 text-label-medium text-foreground">
      {children}
      {hint ? <span className="block text-label-small text-muted-foreground">{hint}</span> : null}
    </div>
  );
}

function Cell({ children, tone }: { children: ReactNode; tone?: "lane" | "empty" | "risk" }) {
  const toneClass =
    tone === "lane"
      ? "bg-info-subtle text-info-subtle-foreground"
      : tone === "risk"
        ? "bg-destructive-subtle text-destructive-subtle-foreground"
        : tone === "empty"
          ? "bg-background"
          : "bg-card text-card-foreground";
  return <div className={`${toneClass} px-3 py-2 text-body-small`}>{children}</div>;
}

function Line({ label, columns }: { label: string; columns: number }) {
  return (
    <div
      className="border-t-2 border-dashed border-primary text-label-small text-primary px-3 pb-1"
      style={{ gridColumn: `1 / span ${columns + 1}` }}
    >
      {label}
    </div>
  );
}

function List({ items }: { items: string[] }) {
  return (
    <ul className="list-disc pl-4">
      {items.map((item) => (
        <li key={item}>
          <CitationText text={item} />
        </li>
      ))}
    </ul>
  );
}

export function BlueprintGrid({ blueprint }: { blueprint?: unknown }) {
  const parsed = BlueprintSchema.safeParse(blueprint);
  if (!parsed.success) {
    return <pre className="text-destructive">{parsed.error.message}</pre>;
  }
  const { actors, moments_of_truth: moments, steps } = parsed.data;
  const customer = actors.find((a) => a.role === "customer")?.name ?? "Customer";
  const motSteps = new Map<number, string>();
  for (const m of moments) motSteps.set(m.step, m.title);
  const laneOf = (step: Step): string => step.actor ?? customer;
  const columns = steps.length;
  const template = `minmax(9rem, 10rem) repeat(${columns}, minmax(13rem, 1fr))`;

  const layerRow = (key: "evidence" | "frontstage" | "backstage" | "support") =>
    steps.map((step) => (
      <Cell key={`${key}-${step.number}`}>
        <CitationText text={step.layers[key] ?? ""} />
      </Cell>
    ));

  return (
    <div className="overflow-x-auto rounded-lg border border-border">
      <div className="grid gap-px bg-border" style={{ gridTemplateColumns: template }}>
        <RowLabel>Step</RowLabel>
        {steps.map((step) => {
          const mot = motSteps.get(step.number);
          return (
            <div
              key={`head-${step.number}`}
              className={`px-3 py-2 ${mot ? "bg-warning-subtle text-warning-subtle-foreground" : "bg-card text-card-foreground"}`}
            >
              <span className="block text-label-small text-muted-foreground">Step {step.number}</span>
              <span className="block text-title-small">{step.title}</span>
              {step.time ? <span className="block text-label-small text-muted-foreground">{step.time}</span> : null}
              <span className="mt-1 flex flex-wrap gap-1">
                {step.emotional_label ? (
                  <Badge variant="outline" size="sm">
                    {step.emotional_label} ({step.emotional_score ?? 0})
                  </Badge>
                ) : null}
                {mot ? (
                  <Badge variant="warning" size="sm" title={mot}>
                    Moment of truth
                  </Badge>
                ) : null}
              </span>
            </div>
          );
        })}

        <RowLabel hint="what the actor sees">Evidence</RowLabel>
        {layerRow("evidence")}

        {actors.map((actor) => (
          <div key={`lane-${actor.name}`} className="contents">
            <RowLabel hint={actor.role === "customer" ? "customer lane" : "on-stage lane"}>{actor.name}</RowLabel>
            {steps.map((step) =>
              laneOf(step) === actor.name ? (
                <Cell key={`lane-${actor.name}-${step.number}`} tone="lane">
                  <CitationText text={step.layers.customer_actions ?? ""} />
                </Cell>
              ) : (
                <Cell key={`lane-${actor.name}-${step.number}`} tone="empty">
                  {""}
                </Cell>
              ),
            )}
          </div>
        ))}

        <Line label="line of interaction" columns={columns} />
        <RowLabel>Frontstage</RowLabel>
        {layerRow("frontstage")}
        <Line label="line of visibility" columns={columns} />
        <RowLabel>Backstage</RowLabel>
        {layerRow("backstage")}
        <Line label="line of internal interaction" columns={columns} />
        <RowLabel>Support</RowLabel>
        {layerRow("support")}

        <RowLabel hint="pinned to the step">Frictions</RowLabel>
        {steps.map((step) => (
          <Cell key={`fr-${step.number}`} tone={step.frictions.length > 0 ? "risk" : "empty"}>
            <List items={step.frictions} />
          </Cell>
        ))}
        <RowLabel hint="pinned to the step">Fail points</RowLabel>
        {steps.map((step) => (
          <Cell key={`fp-${step.number}`} tone={step.fail_points.length > 0 ? "risk" : "empty"}>
            <List items={step.fail_points} />
          </Cell>
        ))}
      </div>
    </div>
  );
}
