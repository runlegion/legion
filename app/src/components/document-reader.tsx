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
            {meta.revision && (
              <>
                <p>Revision</p>
                <p>{meta.revision}</p>
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

function GenericSectionView({
  section,
  documentId,
}: {
  section: GenericSection;
  documentId: string;
}) {
  switch (section.kind) {
    case "paragraph":
      return (
        <>
          <h2>{section.label}</h2>
          <p>{section.value}</p>
        </>
      );
    case "list":
      return (
        <>
          <h2>{section.label}</h2>
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
          <h2>{section.label}</h2>
          <Grid preset="linear">
            {section.value.map((entry, index) => (
              <Card key={`${documentId}-${section.key}-${index}`}>
                <CardContent>
                  <Grid preset="linear">
                    {Object.entries(entry).map(([field, fieldValue]) => (
                      <FieldPair key={field} field={field} value={fieldValue} />
                    ))}
                  </Grid>
                </CardContent>
              </Card>
            ))}
          </Grid>
        </>
      );
    case "object":
      return (
        <>
          <h2>{section.label}</h2>
          <Grid preset="linear">
            {Object.entries(section.value).map(([field, fieldValue]) => (
              <FieldPair key={field} field={field} value={fieldValue} />
            ))}
          </Grid>
        </>
      );
    default:
      return null;
  }
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
