import { useState } from "react";
import type { AcceptResult, FetchFailure } from "@/hooks/use-documents";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { Container } from "@/components/ui/container";
import { Grid } from "@/components/ui/grid";
import { Separator } from "@/components/ui/separator";
import {
  type Document,
  documentMeta,
  extractSections,
  type GenericSection,
  humanizeKey,
  parsePayload,
  sectionFor,
  statusBadgeVariant,
} from "@/lib/documents";

interface DocumentReaderProps {
  document: Document | null;
  fetchError: FetchFailure | null;
  acceptDocument: (id: string) => Promise<AcceptResult>;
}

export function DocumentReader({ document, fetchError, acceptDocument }: DocumentReaderProps) {
  return (
    <Container as="main">
      {fetchError && (
        <Container as="article">
          <p role="alert">
            {fetchError.request} failed: {fetchError.message}
          </p>
        </Container>
      )}

      {document ? (
        <DocumentBody document={document} acceptDocument={acceptDocument} />
      ) : (
        <Container as="article">
          <p>Select a document to review it.</p>
        </Container>
      )}
    </Container>
  );
}

function DocumentBody({
  document,
  acceptDocument,
}: {
  document: Document;
  acceptDocument: (id: string) => Promise<AcceptResult>;
}) {
  const [showRaw, setShowRaw] = useState(false);
  const [acceptError, setAcceptError] = useState<string | null>(null);
  const [accepting, setAccepting] = useState(false);

  const payload = parsePayload(document.payload);
  const meta = documentMeta(document, payload);
  const { promoted, remaining } = extractSections(payload);

  const handleAccept = async () => {
    setAccepting(true);
    setAcceptError(null);
    const result = await acceptDocument(document.id);
    setAccepting(false);
    if (!result.ok) {
      setAcceptError(result.message);
    }
  };

  return (
    <Container as="article" key={document.id}>
      <Card>
        <CardHeader>
          <CardTitle>Meta</CardTitle>
        </CardHeader>
        <CardContent>
          <Grid preset="linear">
            <p>ID</p>
            <p>{meta.id}</p>
            <p>Kind</p>
            <p>{meta.kind}</p>
            <p>Status</p>
            <p>
              <Badge variant={statusBadgeVariant(meta.status)}>{meta.status}</Badge>
            </p>
            {meta.priority && (
              <>
                <p>Priority</p>
                <p>{meta.priority}</p>
              </>
            )}
            <p>Owner</p>
            <p>{meta.owner}</p>
            {meta.date && (
              <>
                <p>Date</p>
                <p>{meta.date}</p>
              </>
            )}
            {meta.author && (
              <>
                <p>Author</p>
                <p>{meta.author}</p>
              </>
            )}
          </Grid>
        </CardContent>
      </Card>

      {document.status === "draft" && (
        <Container as="div">
          <Button
            type="button"
            variant="primary"
            disabled={accepting}
            onClick={() => void handleAccept()}
          >
            {accepting ? "Accepting..." : "Accept"}
          </Button>
          {acceptError && <p role="alert">{acceptError}</p>}
        </Container>
      )}

      <h1>{promoted.title ?? document.id}</h1>

      {promoted.description && <p>{promoted.description}</p>}

      {promoted.acceptance && promoted.acceptance.length > 0 && (
        <>
          <h2>Acceptance</h2>
          <ul>
            {promoted.acceptance.map((item, index) => (
              <li key={`${document.id}-acceptance-${index}`}>{item}</li>
            ))}
          </ul>
        </>
      )}

      {promoted.tracesTo && (
        <>
          <h2>Traces to</h2>
          {Array.isArray(promoted.tracesTo) ? (
            <ul>
              {promoted.tracesTo.map((item, index) => (
                <li key={`${document.id}-traces-${index}`}>{item}</li>
              ))}
            </ul>
          ) : (
            <p>{promoted.tracesTo}</p>
          )}
        </>
      )}

      {promoted.nfrRefs && promoted.nfrRefs.length > 0 && (
        <>
          <h2>Quality requirement references</h2>
          <ul>
            {promoted.nfrRefs.map((ref) => (
              <li key={ref}>{ref}</li>
            ))}
          </ul>
        </>
      )}

      {remaining.map((section) => (
        <GenericSectionView key={section.key} section={section} documentId={document.id} />
      ))}

      <Separator />

      <Button type="button" variant="ghost" size="sm" onClick={() => setShowRaw((value) => !value)}>
        {showRaw ? "Hide raw" : "Show raw"}
      </Button>

      {showRaw && (
        <pre>
          <code>{JSON.stringify(payload, null, 2)}</code>
        </pre>
      )}
    </Container>
  );
}

const HEADING_TAGS = ["h1", "h2", "h3", "h4", "h5", "h6"] as const;

function SectionHeading({ level, children }: { level: number; children: string }) {
  const Tag = HEADING_TAGS[Math.min(Math.max(level, 1), HEADING_TAGS.length) - 1];
  return <Tag>{children}</Tag>;
}

function GenericSectionView({
  section,
  documentId,
  level = 2,
}: {
  section: GenericSection;
  documentId: string;
  level?: number;
}) {
  switch (section.kind) {
    case "paragraph":
      return (
        <>
          <SectionHeading level={level}>{section.label}</SectionHeading>
          <p>{section.value}</p>
        </>
      );
    case "list":
      return (
        <>
          <SectionHeading level={level}>{section.label}</SectionHeading>
          <ul>
            {section.value.map((item, index) => (
              <li key={`${documentId}-${section.key}-${index}`}>{item}</li>
            ))}
          </ul>
        </>
      );
    case "object-list":
      return (
        <>
          <SectionHeading level={level}>{section.label}</SectionHeading>
          <Grid preset="linear">
            {section.value.map((entry, index) => (
              <Card key={`${documentId}-${section.key}-${index}`}>
                <CardContent>
                  <ObjectFields
                    documentId={`${documentId}-${section.key}-${index}`}
                    value={entry}
                    level={level + 1}
                  />
                </CardContent>
              </Card>
            ))}
          </Grid>
        </>
      );
    case "object":
      return (
        <>
          <SectionHeading level={level}>{section.label}</SectionHeading>
          <ObjectFields
            documentId={`${documentId}-${section.key}`}
            value={section.value}
            level={level + 1}
          />
        </>
      );
    default:
      return null;
  }
}

/**
 * Renders an object's fields: scalars and string arrays as label/value pairs
 * inside one Grid, nested objects and arrays of objects as their own
 * full-width sections below it (one heading level deeper) rather than
 * stringifying them or squeezing a nested Grid into a single grid cell.
 */
function ObjectFields({
  documentId,
  value,
  level,
}: {
  documentId: string;
  value: Record<string, unknown>;
  level: number;
}) {
  const scalarFields: Array<[string, unknown]> = [];
  const nestedSections: GenericSection[] = [];

  for (const [field, fieldValue] of Object.entries(value)) {
    const nested = sectionFor(field, fieldValue);
    if (nested.kind === "object" || nested.kind === "object-list") {
      nestedSections.push(nested);
    } else {
      scalarFields.push([field, fieldValue]);
    }
  }

  return (
    <>
      {scalarFields.length > 0 && (
        <Container as="div">
          <Grid preset="linear">
            {scalarFields.map(([field, fieldValue]) => (
              <FieldPair key={field} field={field} value={fieldValue} />
            ))}
          </Grid>
        </Container>
      )}
      {nestedSections.map((section) => (
        <GenericSectionView
          key={section.key}
          section={section}
          documentId={`${documentId}-${section.key}`}
          level={level}
        />
      ))}
    </>
  );
}

function FieldPair({ field, value }: { field: string; value: unknown }) {
  const text = Array.isArray(value) ? value.join(", ") : value == null ? "" : String(value);
  return (
    <>
      <p>{humanizeKey(field)}</p>
      <p>{text}</p>
    </>
  );
}
