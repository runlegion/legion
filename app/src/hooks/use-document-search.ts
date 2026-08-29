import { useCallback, useEffect, useRef, useState } from "react";
import { z } from "zod";
import { type FetchFailure, failure, requestJson } from "@/lib/api";
import { type Document, DocumentListSchema } from "@/lib/documents";

/** Keystrokes rest this long before the index is queried. */
const QUERY_DEBOUNCE_MS = 250;

/**
 * GET /api/search (#1037) may answer with the document array itself or wrap it
 * under a results key; accept either rather than guessing one.
 */
const SearchResponseSchema = z.union([
  DocumentListSchema,
  z.object({ results: DocumentListSchema }).transform((value) => value.results),
  z.object({ documents: DocumentListSchema }).transform((value) => value.documents),
]);

export interface UseDocumentSearchResult {
  query: string;
  setQuery: (q: string) => void;
  results: Document[];
  loading: boolean;
  error: FetchFailure | null;
}

/** Search over the repo's documents in the daemon's index. */
export function useDocumentSearch(repo: string): UseDocumentSearchResult {
  const [query, setQuery] = useState("");
  const [results, setResults] = useState<Document[]>([]);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<FetchFailure | null>(null);
  const controller = useRef<AbortController | null>(null);

  useEffect(() => {
    const trimmed = query.trim();
    if (trimmed.length === 0) {
      controller.current?.abort();
      controller.current = null;
      setResults([]);
      setLoading(false);
      setError(null);
      return;
    }

    setLoading(true);
    const timer = setTimeout(() => {
      controller.current?.abort();
      const next = new AbortController();
      controller.current = next;
      const path = `/api/search?q=${encodeURIComponent(trimmed)}&repo=${encodeURIComponent(repo)}`;

      void (async () => {
        try {
          setResults(await requestJson(path, SearchResponseSchema, { signal: next.signal }));
          setError(null);
        } catch (err) {
          if (next.signal.aborted) return;
          setResults([]);
          setError(failure(`GET ${path}`, err));
        } finally {
          if (!next.signal.aborted) setLoading(false);
        }
      })();
    }, QUERY_DEBOUNCE_MS);

    return () => clearTimeout(timer);
  }, [query, repo]);

  useEffect(() => {
    return () => controller.current?.abort();
  }, []);

  const update = useCallback((next: string) => setQuery(next), []);

  return { query, setQuery: update, results, loading, error };
}
