// Typed access to the legion document + kanban HTTP surface (channel::router,
// issues #702/#700). The dev server proxies these to a running legion on
// :3131; in production the same binary that exposes the endpoints serves this
// bundle, so the paths are identical either way.

/// A document row as returned by GET /api/documents. `payload` is the raw
/// JSON string of the type-specific document body; callers parse it per type.
export interface DocumentRow {
  id: string;
  doc_type: string;
  surface: string | null;
  status: string;
  priority: string | null;
  owner: string;
  payload: string;
  archived_at: string | null;
  created_at: string;
  updated_at: string;
}

async function getJson<T>(url: string): Promise<T> {
  const res = await fetch(url);
  if (!res.ok) {
    throw new Error(`${res.status} ${res.statusText} for ${url}`);
  }
  return (await res.json()) as T;
}

/// List documents, optionally filtered. Empty filter returns the hot set.
export async function listDocuments(filter: {
  doc_type?: string;
  surface?: string;
  status?: string;
  owner?: string;
} = {}): Promise<DocumentRow[]> {
  const q = new URLSearchParams();
  for (const [k, v] of Object.entries(filter)) {
    if (v) q.set(k, v);
  }
  const qs = q.toString();
  return getJson<DocumentRow[]>(`/api/documents${qs ? `?${qs}` : ""}`);
}

/// One document by id.
export async function getDocument(id: string): Promise<DocumentRow> {
  return getJson<DocumentRow>(`/api/documents/${encodeURIComponent(id)}`);
}

/// Flip a document's lifecycle status (the Publish/Approve action). Returns
/// the updated row.
export async function setDocumentStatus(
  id: string,
  to: string,
): Promise<DocumentRow> {
  const res = await fetch(`/api/documents/${encodeURIComponent(id)}/status`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ to }),
  });
  if (!res.ok) {
    throw new Error(`publish failed: ${res.status} ${res.statusText}`);
  }
  return (await res.json()) as DocumentRow;
}
