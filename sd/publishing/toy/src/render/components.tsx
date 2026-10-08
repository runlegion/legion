/**
 * The components map rafters' `toJsx` resolves block types against.
 *
 * Every rafters entry is the installed component itself, adapted only where
 * its public shape is a namespace (`Grid.Item`, `Table.Row`) rather than a
 * plain export. The legion entries are the short list of things a composite
 * could not draw; each file says why.
 */
import type { ComponentType, ReactNode } from "react";
import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert";
import { Badge } from "@/components/ui/badge";
import { Card, CardContent, CardDescription, CardFooter, CardHeader, CardTitle } from "@/components/ui/card";
import { Container } from "@/components/ui/container";
import { Grid } from "@/components/ui/grid";
import { Separator } from "@/components/ui/separator";
import { Table, TableBody, TableCaption, TableCell, TableHead, TableHeader, TableRow } from "@/components/ui/table";
import { Blockquote, H1, H2, H3, H4, Lead, Li, Muted, P, Small, Ul } from "@/components/ui/typography";
import { BlueprintGrid } from "@/legion/blueprint-grid";
import { CitationChips, CitationText } from "@/legion/citations";
import { EcosystemRings } from "@/legion/ecosystem-rings";
import { EmotionCurve } from "@/legion/emotion-curve";

// toJsx hands every component `Record<string, unknown>`; the installed
// components declare their own prop types. This is the one widening seam.
type Loose = ComponentType<Record<string, unknown>>;
function loose<P>(component: ComponentType<P>): Loose {
  return component as unknown as Loose;
}

function Text({ children }: { children?: ReactNode }) {
  return <>{children}</>;
}

function Link({ href, children }: { href?: string; children?: ReactNode }) {
  return (
    <a href={href} className="text-primary underline-offset-4 hover:underline break-all">
      {children}
    </a>
  );
}

export const components: Record<string, Loose> = {
  // rafters
  container: loose(Container),
  grid: loose(Grid),
  "grid-item": loose(Grid.Item),
  card: loose(Card),
  "card-header": loose(CardHeader),
  "card-title": loose(CardTitle),
  "card-description": loose(CardDescription),
  "card-content": loose(CardContent),
  "card-footer": loose(CardFooter),
  badge: loose(Badge),
  table: loose(Table),
  "table-header": loose(TableHeader),
  "table-body": loose(TableBody),
  "table-row": loose(TableRow),
  "table-head": loose(TableHead),
  "table-cell": loose(TableCell),
  "table-caption": loose(TableCaption),
  alert: loose(Alert),
  "alert-title": loose(AlertTitle),
  "alert-description": loose(AlertDescription),
  separator: loose(Separator),
  h1: loose(H1),
  h2: loose(H2),
  h3: loose(H3),
  h4: loose(H4),
  p: loose(P),
  lead: loose(Lead),
  muted: loose(Muted),
  small: loose(Small),
  blockquote: loose(Blockquote),
  ul: loose(Ul),
  li: loose(Li),
  // native
  text: loose(Text),
  link: loose(Link),
  // legion
  "citation-text": loose(CitationText),
  "citation-chips": loose(CitationChips),
  "emotion-curve": loose(EmotionCurve),
  "blueprint-grid": loose(BlueprintGrid),
  "ecosystem-rings": loose(EcosystemRings),
};

/** Shown for any block type the map lacks, so a gap is visible on the page. */
export function MissingBlock({ type }: { type: string }) {
  return <span className="bg-destructive text-destructive-foreground px-1">missing block type: {type}</span>;
}
