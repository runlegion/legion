import { atom } from "nanostores";
import { useCallback, useEffect, useRef, useState, useSyncExternalStore } from "react";
import { type FetchFailure, documentPath, failure, requestJson, requestOk } from "@/lib/api";
import { type Document, DocumentSchema } from "@/lib/documents";
import { documentBody, payloadObject } from "@/lib/tree";

/** How long the editor rests before the working copy is written back. */
const SAVE_DEBOUNCE_MS = 750;

export interface UseDocumentSaveResult {
  /** Current working-copy body text (nanostore-backed). */
  body: string;
  setBody: (next: string) => void;
  /**
   * Awaits any in-flight or pending debounced body save. Call before revise()
   * or a status-change revise so a stale snapshot never clobbers the last
   * keystroke.
   */
  flushPendingSave: () => Promise<void>;
  /**
   * POST .../revise with the current working copy (body + any settings edits
   * merged in). Flushes the pending body save first.
   */
  revise: (settingsPatch?: Record<string, unknown>) => Promise<Document>;
  saving: boolean;
  error: FetchFailure | null;
  /**
   * True while the working copy differs from the body of the last revision.
   * A debounced body save clears no revision, so this stays true until Revise
   * or a status change cuts one -- it is what the leave prompt asks about.
   */
  hasUnrevisedEdits: boolean;
}

/**
 * The editor's working copy and its two write paths: a debounced body save that
 * bumps no revision, and an explicit revise that cuts one.
 */
export function useDocumentSave(doc: Document): UseDocumentSaveResult {
  // One store per document: the pane is keyed on the document id, so a new
  // document remounts this hook and starts a fresh working copy.
  const [store] = useState(() => atom(documentBody(doc)));

  const body = useSyncExternalStore(
    useCallback((onChange: () => void) => store.subscribe(onChange), [store]),
    useCallback(() => store.get(), [store]),
  );

  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<FetchFailure | null>(null);
  const [hasUnrevisedEdits, setHasUnrevisedEdits] = useState(false);

  const timer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const inFlight = useRef<Promise<void> | null>(null);

  const saveBody = useCallback(async (): Promise<void> => {
    const path = documentPath(doc.id, "/body");
    setSaving(true);
    try {
      await requestOk(path, { method: "PUT", body: { body: store.get() } });
      setError(null);
    } catch (err) {
      setError(failure(`PUT ${path}`, err));
    } finally {
      setSaving(false);
    }
  }, [doc.id, store]);

  const runSave = useCallback((): Promise<void> => {
    const pending = saveBody().finally(() => {
      if (inFlight.current === pending) inFlight.current = null;
    });
    inFlight.current = pending;
    return pending;
  }, [saveBody]);

  const setBody = useCallback(
    (next: string) => {
      store.set(next);
      setHasUnrevisedEdits(true);
      if (timer.current !== null) clearTimeout(timer.current);
      timer.current = setTimeout(() => {
        timer.current = null;
        void runSave();
      }, SAVE_DEBOUNCE_MS);
    },
    [store, runSave],
  );

  const flushPendingSave = useCallback(async (): Promise<void> => {
    if (timer.current !== null) {
      clearTimeout(timer.current);
      timer.current = null;
      await runSave();
      return;
    }
    if (inFlight.current) await inFlight.current;
  }, [runSave]);

  const revise = useCallback(
    async (settingsPatch?: Record<string, unknown>): Promise<Document> => {
      await flushPendingSave();
      const path = documentPath(doc.id, "/revise");
      setSaving(true);
      try {
        const base = payloadObject(doc) ?? {};
        const working = store.get();
        const payload: Record<string, unknown> = { ...base, ...(settingsPatch ?? {}) };
        // Only editing creates the body key. A status change revises documents
        // that never had one -- 187 of the 224 current documents, including the
        // schema documents an empty body would invalidate -- so an untouched
        // empty working copy must not write the key into them.
        if (working.length > 0 || "body" in base) payload["body"] = working;
        // The revise endpoint takes the payload as the request body itself,
        // not wrapped under a "payload" key -- sending it wrapped stores the
        // wrapper as the payload (verified live against the #1036 daemon).
        const next = await requestJson(path, DocumentSchema, { method: "POST", body: payload });
        setError(null);
        setHasUnrevisedEdits(false);
        return next;
      } catch (err) {
        const recorded = failure(`POST ${path}`, err);
        setError(recorded);
        throw err;
      } finally {
        setSaving(false);
      }
    },
    [doc, store, flushPendingSave],
  );

  // Dropping a document mid-debounce would silently lose the last keystrokes.
  useEffect(() => {
    return () => {
      if (timer.current !== null) {
        clearTimeout(timer.current);
        timer.current = null;
        void runSave();
      }
    };
  }, [runSave]);

  return { body, setBody, flushPendingSave, revise, saving, error, hasUnrevisedEdits };
}
