import { useCallback, useEffect, useState } from "react";
import { type Document, DocumentListSchema, DocumentSchema } from "@/lib/documents";

export interface FetchFailure {
  request: string;
  message: string;
}

export type AcceptResult = { ok: true; document: Document } | { ok: false; message: string };

export interface UseDocumentsResult {
  documents: Document[];
  loading: boolean;
  /** Persists until the next successful fetch of /api/documents. */
  error: FetchFailure | null;
  refetch: () => void;
  acceptDocument: (id: string) => Promise<AcceptResult>;
}

function describeError(err: unknown): string {
  if (err instanceof Error) return err.message;
  return "unknown error";
}

/** The one data-access hook over the daemon's /api/documents surface. */
export function useDocuments(): UseDocumentsResult {
  const [documents, setDocuments] = useState<Document[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<FetchFailure | null>(null);

  const fetchDocuments = useCallback(() => {
    setLoading(true);
    void (async () => {
      try {
        const res = await fetch("/api/documents");
        if (!res.ok) {
          throw new Error(`request failed with status ${res.status}`);
        }
        const json: unknown = await res.json();
        const parsed = DocumentListSchema.parse(json);
        setDocuments(parsed);
        setError(null);
      } catch (err) {
        setError({ request: "GET /api/documents", message: describeError(err) });
      } finally {
        setLoading(false);
      }
    })();
  }, []);

  useEffect(() => {
    fetchDocuments();
  }, [fetchDocuments]);

  const acceptDocument = useCallback(async (id: string): Promise<AcceptResult> => {
    try {
      const res = await fetch(`/api/documents/${encodeURIComponent(id)}/status`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ to: "accepted" }),
      });
      if (!res.ok) {
        return {
          ok: false,
          message: `POST /api/documents/${id}/status failed with status ${res.status}`,
        };
      }
      const json: unknown = await res.json();
      const parsed = DocumentSchema.parse(json);
      setDocuments((prev) => prev.map((doc) => (doc.id === parsed.id ? parsed : doc)));
      return { ok: true, document: parsed };
    } catch (err) {
      return { ok: false, message: describeError(err) };
    }
  }, []);

  return { documents, loading, error, refetch: fetchDocuments, acceptDocument };
}
