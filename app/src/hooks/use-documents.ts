import { useCallback, useEffect, useState } from "react";
import { type FetchFailure, failure, requestJson, documentPath } from "@/lib/api";
import { type Document, DocumentListSchema, DocumentSchema } from "@/lib/documents";

export type { FetchFailure } from "@/lib/api";

export type MutationResult =
  | { ok: true; document: Document }
  | { ok: false; failure: FetchFailure };

export interface UseDocumentsResult {
  documents: Document[];
  loading: boolean;
  /** Persists until the next successful fetch of /api/documents. */
  error: FetchFailure | null;
  refetch: () => void;
  /** Replace one document in the loaded list after it is revised elsewhere. */
  replaceDocument: (next: Document) => void;
  /** POST /api/documents/{id}/status -- the general status setter. */
  setDocumentStatus: (id: string, status: string) => Promise<MutationResult>;
  /** POST /api/documents -- creates a new intention (#1036). */
  createDocument: (docType: string, owner: string) => Promise<MutationResult>;
}

/** The one data-access hook over the daemon's /api/documents collection. */
export function useDocuments(): UseDocumentsResult {
  const [documents, setDocuments] = useState<Document[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<FetchFailure | null>(null);

  const fetchDocuments = useCallback(() => {
    setLoading(true);
    void (async () => {
      try {
        setDocuments(await requestJson("/api/documents", DocumentListSchema));
        setError(null);
      } catch (err) {
        setError(failure("GET /api/documents", err));
      } finally {
        setLoading(false);
      }
    })();
  }, []);

  useEffect(() => {
    fetchDocuments();
  }, [fetchDocuments]);

  const replaceDocument = useCallback((next: Document) => {
    setDocuments((prev) => prev.map((doc) => (doc.id === next.id ? next : doc)));
  }, []);

  const setDocumentStatus = useCallback(
    async (id: string, status: string): Promise<MutationResult> => {
      const request = `POST ${documentPath(id, "/status")}`;
      try {
        const document = await requestJson(documentPath(id, "/status"), DocumentSchema, {
          method: "POST",
          body: { to: status },
        });
        replaceDocument(document);
        return { ok: true, document };
      } catch (err) {
        return { ok: false, failure: failure(request, err) };
      }
    },
    [replaceDocument],
  );

  const createDocument = useCallback(
    async (docType: string, owner: string): Promise<MutationResult> => {
      try {
        const document = await requestJson("/api/documents", DocumentSchema, {
          method: "POST",
          body: { doc_type: docType, owner, payload: {} },
        });
        setDocuments((prev) => [...prev, document]);
        return { ok: true, document };
      } catch (err) {
        return { ok: false, failure: failure("POST /api/documents", err) };
      }
    },
    [],
  );

  return {
    documents,
    loading,
    error,
    refetch: fetchDocuments,
    replaceDocument,
    setDocumentStatus,
    createDocument,
  };
}
