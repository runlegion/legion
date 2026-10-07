/**
 * CitationText and CitationChips: legion components.
 *
 * Why not a composite: every document except the Discovery carries its
 * citations as `[C23]`, `[C5, C11]` or `(C14)` tokens inside prose fields, not as a
 * structured `citations: string[]` beside the text. A composite can bind a
 * field but cannot split a string into text runs and links. Resolving an id to
 * its quote, author and source also needs a join against the Discovery's
 * `citations[]`, which `$bind` (a path into one payload) cannot express.
 *
 * This is the one place in the toy that reads prose. It is a schema gap: with
 * structured per-field citation ids the chips become a `repeat` over ids and
 * the text a plain bind.
 */
import { createContext, type ReactNode, useContext } from "react";
import { HoverCard } from "@/components/ui/hover-card";

export interface Citation {
  id: string;
  url: string;
  date: string;
  author: string;
  family: string;
  text: string;
}

const CitationContext = createContext<ReadonlyMap<string, Citation>>(new Map());

export function CitationProvider({
  citations,
  children,
}: {
  citations: ReadonlyMap<string, Citation>;
  children: ReactNode;
}) {
  return <CitationContext.Provider value={citations}>{children}</CitationContext.Provider>;
}

const CITATION_ID = /\bC\d+\b/g;

function hrefFor(citation: Citation): string | undefined {
  return citation.url.startsWith("http") ? citation.url : undefined;
}

export function CitationLink({ id }: { id: string }) {
  const citations = useContext(CitationContext);
  const citation = citations.get(id);
  if (!citation) {
    return <span className="text-muted-foreground font-mono text-label-small">{id}</span>;
  }
  return (
    <HoverCard>
      <HoverCard.Trigger
        href={hrefFor(citation)}
        className="font-mono text-label-small text-primary underline-offset-2 hover:underline"
        title={`${citation.author}, ${citation.date}: ${citation.text}`}
      >
        {id}
      </HoverCard.Trigger>
      <HoverCard.Content>
        <span className="block text-label-medium text-foreground">"{citation.text}"</span>
        <span className="block text-label-small text-muted-foreground">
          {citation.author}, {citation.date} ({citation.family})
        </span>
      </HoverCard.Content>
    </HoverCard>
  );
}

export function CitationChips({ ids }: { ids?: unknown }) {
  if (!Array.isArray(ids) || ids.length === 0) return null;
  const parts: ReactNode[] = [];
  for (const [index, id] of ids.entries()) {
    if (typeof id !== "string") continue;
    if (index > 0) parts.push(" ");
    parts.push(<CitationLink key={id} id={id} />);
  }
  return <span className="[&>div]:inline-block">{parts}</span>;
}

/**
 * Renders a prose field with every known citation id in it turned into a
 * link. Documents disagree on the wrapper ("[C5, C11]" in personas and
 * journeys, "(C14)" in Discovery answers), so the scan is for the bare id,
 * and only ids the Discovery register knows become links.
 */
export function CitationText({ text }: { text?: unknown }) {
  const citations = useContext(CitationContext);
  if (typeof text !== "string" || text.length === 0) return null;
  const parts: ReactNode[] = [];
  let last = 0;
  for (const match of text.matchAll(CITATION_ID)) {
    const id = match[0];
    if (!citations.has(id)) continue;
    parts.push(text.slice(last, match.index));
    parts.push(<CitationLink key={`${match.index}`} id={id} />);
    last = match.index + id.length;
  }
  parts.push(text.slice(last));
  return <span className="[&>div]:inline-block">{parts}</span>;
}
