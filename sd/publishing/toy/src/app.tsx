import { useEffect, useState } from "react";
import { z } from "zod";
import { Container } from "@/components/ui/container";
import blueprintComposite from "@/composites/service-blueprint.composite.json";
import discoveryComposite from "@/composites/question-board.composite.json";
import ecosystemComposite from "@/composites/ecosystem-map.composite.json";
import intentComposite from "@/composites/intent-sheet.composite.json";
import journeyComposite from "@/composites/journey-map.composite.json";
import personaComposite from "@/composites/persona-card.composite.json";
import { type Citation, CitationProvider } from "@/legion/citations";
import { DocumentView } from "@/render/document-view";
import blueprint from "../data/blueprint.json";
import discovery from "../data/discovery.json";
import ecosystem from "../data/ecosystem.json";
import intent from "../data/intent.json";
import journey from "../data/journey.json";
import persona from "../data/persona.json";

const DOCUMENTS = [
  { key: "intent", label: "Intent", composite: intentComposite, payload: intent },
  { key: "discovery", label: "Discovery", composite: discoveryComposite, payload: discovery },
  { key: "persona", label: "Persona", composite: personaComposite, payload: persona },
  { key: "journey", label: "Journey", composite: journeyComposite, payload: journey },
  { key: "blueprint", label: "Blueprint", composite: blueprintComposite, payload: blueprint },
  { key: "ecosystem", label: "Ecosystem", composite: ecosystemComposite, payload: ecosystem },
] as const;

const CitationListSchema = z.array(
  z.object({
    id: z.string(),
    url: z.string(),
    date: z.string(),
    author: z.string(),
    family: z.string(),
    text: z.string(),
  }),
);

function buildCitations(): ReadonlyMap<string, Citation> {
  const list = CitationListSchema.parse(discovery.citations);
  const map = new Map<string, Citation>();
  for (const citation of list) map.set(citation.id, citation);
  return map;
}

const citations = buildCitations();

function currentKey(): string {
  const hash = window.location.hash.replace(/^#/, "");
  return DOCUMENTS.some((d) => d.key === hash) ? hash : "intent";
}

export function App() {
  const [key, setKey] = useState(currentKey);
  useEffect(() => {
    const onHash = () => setKey(currentKey());
    window.addEventListener("hashchange", onHash);
    return () => window.removeEventListener("hashchange", onHash);
  }, []);
  const doc = DOCUMENTS.find((d) => d.key === key) ?? DOCUMENTS[0];

  return (
    <CitationProvider citations={citations}>
      <Container as="header" size="full" padding="4">
        <nav aria-label="Documents" className="flex flex-wrap gap-4 border-b border-border pb-2">
          {DOCUMENTS.map((d) => (
            <a
              key={d.key}
              href={`#${d.key}`}
              aria-current={d.key === key ? "page" : undefined}
              className={
                d.key === key
                  ? "text-label-large text-foreground underline underline-offset-8"
                  : "text-label-large text-muted-foreground hover:text-foreground"
              }
            >
              {d.label}
            </a>
          ))}
        </nav>
      </Container>
      <main data-document={doc.key}>
        <DocumentView composite={doc.composite} payload={doc.payload} />
      </main>
    </CitationProvider>
  );
}
