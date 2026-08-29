import { type Document, parsePayload } from "@/lib/documents";

/**
 * A work item recorded in a spec's trace line. Rendered as a link to the work
 * source itself -- never as a document node, because no document backs it.
 */
export interface WorkSourceLink {
  kind: "issue" | "pr";
  number: number;
  url: string;
  traceLine: string;
}

export interface SpecNode {
  document: Document;
  workSources: WorkSourceLink[];
}

export interface ThesisNode {
  document: Document;
  title: string;
  specs: SpecNode[];
}

export interface RepoTree {
  repo: string;
  serviceDesignDocs: Document[];
  intentions: ThesisNode[];
}

/** doc_types the settled design groups under the Service Design node. */
const SERVICE_DESIGN_TYPES: ReadonlySet<string> = new Set([
  "persona",
  "journey",
  "blueprint",
  "ecosystem",
  "painmatrix",
]);

export function isServiceDesignType(docType: string): boolean {
  return SERVICE_DESIGN_TYPES.has(docType);
}

function asRecord(value: unknown): Record<string, unknown> | null {
  if (typeof value !== "object" || value === null || Array.isArray(value)) return null;
  return value as Record<string, unknown>;
}

/** The parsed payload object, or null when the payload is absent or not an object. */
export function payloadObject(doc: Document): Record<string, unknown> | null {
  return asRecord(parsePayload(doc.payload));
}

/**
 * A document's display title. Payloads carry it either at the top level or
 * inside `meta`; documents with neither fall back to their id.
 */
export function documentTitle(doc: Document): string {
  const payload = payloadObject(doc);
  if (payload) {
    const top = payload["title"];
    if (typeof top === "string" && top.length > 0) return top;
    const meta = asRecord(payload["meta"]);
    const nested = meta?.["title"];
    if (typeof nested === "string" && nested.length > 0) return nested;
  }
  return doc.id;
}

/** The working-copy body, empty when the document has no `body` key. */
export function documentBody(doc: Document): string {
  const body = payloadObject(doc)?.["body"];
  return typeof body === "string" ? body : "";
}

/**
 * `traces_to` is free text, not an id reference: a requirement records
 * "thesis 01a04092 (OQ-3, OQ-9); smugglr rfc 01a04241 section 3b". Read every
 * recorded form as a list of trace lines.
 */
function traceLines(doc: Document): string[] {
  const traces = payloadObject(doc)?.["traces_to"];
  if (typeof traces === "string") {
    return traces
      .split(";")
      .map((line) => line.trim())
      .filter((line) => line.length > 0);
  }
  if (Array.isArray(traces)) {
    return traces.filter((line): line is string => typeof line === "string");
  }
  return [];
}

/**
 * Trace lines name a thesis by its full UUID or, as legion's own prose does,
 * by its leading segment alone ("thesis 01a04092").
 */
function namesDocument(line: string, targetId: string): boolean {
  if (line.includes(targetId)) return true;
  const leadingSegment = targetId.split("-")[0];
  return (
    leadingSegment !== undefined && leadingSegment.length >= 8 && line.includes(leadingSegment)
  );
}

const WORK_SOURCE_PATTERN = /(issue|pull request|pr)?\s*#(\d+)/gi;

/**
 * The GitHub organisation legion's work source lives under. The daemon exposes
 * no repo-to-slug lookup today, so the org is fixed here rather than guessed
 * per repo; replace it with a served value when one exists.
 */
const WORK_SOURCE_ORG = "runlegion";

function workSourceUrl(repo: string, kind: "issue" | "pr", number: number): string {
  const path = kind === "pr" ? "pull" : "issues";
  return `https://github.com/${WORK_SOURCE_ORG}/${repo}/${path}/${number}`;
}

/**
 * The issue and PR numbers a spec records in its trace lines, each carrying the
 * line it was found in. A bare `#123` reads as an issue, matching legion's own
 * prose convention.
 */
export function workSourcesOf(repo: string, doc: Document): WorkSourceLink[] {
  const links: WorkSourceLink[] = [];
  const seen = new Set<string>();
  for (const line of traceLines(doc)) {
    for (const match of line.matchAll(WORK_SOURCE_PATTERN)) {
      const label = match[1]?.toLowerCase();
      const digits = match[2];
      if (digits === undefined) continue;
      const number = Number.parseInt(digits, 10);
      if (!Number.isSafeInteger(number)) continue;
      const kind: "issue" | "pr" = label === "pr" || label === "pull request" ? "pr" : "issue";
      const key = `${kind}:${number}`;
      if (seen.has(key)) continue;
      seen.add(key);
      links.push({ kind, number, url: workSourceUrl(repo, kind, number), traceLine: line });
    }
  }
  return links;
}

function byTitle(a: { title: string }, b: { title: string }): number {
  return a.title.localeCompare(b.title);
}

/**
 * Derive the sidebar tree for one repo from the already-loaded document list.
 * Pure: it fetches nothing and holds no state.
 */
export function buildRepoTree(repo: string, documents: Document[]): RepoTree {
  const owned = documents.filter((doc) => doc.owner === repo);

  const serviceDesignDocs = owned
    .filter((doc) => isServiceDesignType(doc.doc_type))
    .map((doc) => ({ doc, title: documentTitle(doc) }))
    .sort(byTitle)
    .map((entry) => entry.doc);

  const theses = owned.filter((doc) => doc.doc_type === "thesis");
  const candidates = owned.filter((doc) => doc.doc_type !== "thesis");

  const intentions: ThesisNode[] = theses
    .map((thesis) => {
      const specs: SpecNode[] = candidates
        .filter((doc) => traceLines(doc).some((line) => namesDocument(line, thesis.id)))
        .map((doc) => ({ doc, title: documentTitle(doc) }))
        .sort(byTitle)
        .map((entry) => ({
          document: entry.doc,
          workSources: workSourcesOf(repo, entry.doc),
        }));
      return { document: thesis, title: documentTitle(thesis), specs };
    })
    .sort(byTitle);

  return { repo, serviceDesignDocs, intentions };
}

/**
 * The statuses offered for a document: those already in use by documents of the
 * same doc_type, plus the document's own, so the current value is always
 * selectable. Computed client-side from the loaded list -- no daemon endpoint.
 */
export function statusesForDocType(documents: Document[], doc: Document): string[] {
  const statuses = new Set<string>([doc.status]);
  for (const other of documents) {
    if (other.doc_type === doc.doc_type) statuses.add(other.status);
  }
  return [...statuses].sort();
}
