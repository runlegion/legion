import { z } from "zod";

/**
 * A document as served by the daemon's /api/documents endpoints. The list
 * endpoint omits archived_at/created_at/updated_at; the single-document GET
 * and the status POST include them.
 */
export const DocumentSchema = z.object({
  id: z.string(),
  doc_type: z.string(),
  surface: z.string().nullable(),
  status: z.string(),
  priority: z.string().nullable(),
  owner: z.string(),
  payload: z.string(),
  archived_at: z.string().nullable().optional(),
  created_at: z.string().optional(),
  updated_at: z.string().optional(),
});

export type Document = z.infer<typeof DocumentSchema>;

export const DocumentListSchema = z.array(DocumentSchema);

/** The status filter offered in the navigator. */
export type StatusFilter = "all" | "draft" | "accepted" | "published";

export const STATUS_FILTERS: StatusFilter[] = ["all", "draft", "accepted", "published"];

export const STATUS_FILTER_LABEL: Record<StatusFilter, string> = {
  all: "All",
  draft: "Draft",
  accepted: "Accepted",
  published: "Published",
};

/** The grouping bucket a document's kind falls into, in display order. */
export type KindGroup =
  | "direction"
  | "requirements"
  | "quality-requirements"
  | "research"
  | "rulings"
  | "other";

export const KIND_GROUP_ORDER: KindGroup[] = [
  "direction",
  "requirements",
  "quality-requirements",
  "research",
  "rulings",
  "other",
];

export const KIND_GROUP_LABEL: Record<KindGroup, string> = {
  direction: "Direction",
  requirements: "Requirements",
  "quality-requirements": "Quality requirements",
  research: "Research",
  rulings: "Rulings",
  other: "Other",
};

const KIND_GROUP_BY_DOC_TYPE: Record<string, KindGroup> = {
  thesis: "direction",
  requirement: "requirements",
  nfr: "quality-requirements",
  research: "research",
  ruling: "rulings",
};

export function kindGroupFor(docType: string): KindGroup {
  return KIND_GROUP_BY_DOC_TYPE[docType] ?? "other";
}

// ---------------------------------------------------------------------------
// Payload parsing. The payload column is a JSON string whose shape varies by
// doc_type; we parse it once into `unknown` and narrow with guards rather
// than trusting or casting it.
// ---------------------------------------------------------------------------

export function parsePayload(raw: string): unknown {
  try {
    return JSON.parse(raw) as unknown;
  } catch {
    return null;
  }
}

export function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

/** Just the title guard, for callers (e.g. tree rows) that don't need the full section split. */
export function payloadTitle(payload: unknown): string | null {
  return isRecord(payload) && typeof payload.title === "string" ? payload.title : null;
}

function isStringArray(value: unknown): value is string[] {
  return Array.isArray(value) && value.every((entry) => typeof entry === "string");
}

/** The payload fields the reader promotes to first-class headings. */
export interface PromotedFields {
  title: string | null;
  description: string | null;
  acceptance: string[] | null;
  tracesTo: string | string[] | null;
  nfrRefs: string[] | null;
}

/** One remaining payload section, generically shaped for rendering. */
export type GenericSection =
  | { key: string; label: string; kind: "paragraph"; value: string }
  | { key: string; label: string; kind: "list"; value: string[] }
  | { key: string; label: string; kind: "object-list"; value: Array<Record<string, unknown>> }
  | { key: string; label: string; kind: "object"; value: Record<string, unknown> };

const PROMOTED_KEYS = new Set(["meta", "title", "description", "traces_to", "nfr_refs"]);

/**
 * Split a parsed payload into the fields the reader promotes to first-class
 * headings and everything else, generically shaped for the fallback
 * renderer. Never returns raw JSON text.
 */
export function extractSections(payload: unknown): {
  promoted: PromotedFields;
  remaining: GenericSection[];
} {
  if (!isRecord(payload)) {
    return {
      promoted: { title: null, description: null, acceptance: null, tracesTo: null, nfrRefs: null },
      remaining: [],
    };
  }

  const title = typeof payload.title === "string" ? payload.title : null;
  const description = typeof payload.description === "string" ? payload.description : null;

  const tracesTo =
    typeof payload.traces_to === "string"
      ? payload.traces_to
      : isStringArray(payload.traces_to)
        ? payload.traces_to
        : null;

  const nfrRefs = isStringArray(payload.nfr_refs) ? payload.nfr_refs : null;

  const rest: Record<string, unknown> = {};
  for (const [key, value] of Object.entries(payload)) {
    if (!PROMOTED_KEYS.has(key)) rest[key] = value;
  }

  let acceptance: string[] | null = null;
  if (isStringArray(payload.acceptance)) {
    acceptance = payload.acceptance;
    delete rest.acceptance;
  } else if (isRecord(payload.verification) && isStringArray(payload.verification.acceptance)) {
    acceptance = payload.verification.acceptance;
    const restVerification: Record<string, unknown> = {};
    for (const [key, value] of Object.entries(payload.verification)) {
      if (key !== "acceptance") restVerification[key] = value;
    }
    if (Object.keys(restVerification).length > 0) {
      rest.verification = restVerification;
    } else {
      delete rest.verification;
    }
  }

  const remaining = Object.entries(rest).map(([key, value]) => sectionFor(key, value));

  return { promoted: { title, description, acceptance, tracesTo, nfrRefs }, remaining };
}

export function humanizeKey(key: string): string {
  return key
    .split("_")
    .map((word, index) => (index === 0 ? capitalize(word) : word))
    .join(" ");
}

function capitalize(word: string): string {
  return word.length === 0 ? word : word[0].toUpperCase() + word.slice(1);
}

export function sectionFor(key: string, value: unknown): GenericSection {
  const label = humanizeKey(key);

  if (typeof value === "string") {
    return { key, label, kind: "paragraph", value };
  }
  if (isStringArray(value)) {
    return { key, label, kind: "list", value };
  }
  if (Array.isArray(value)) {
    if (value.every(isRecord)) {
      return { key, label, kind: "object-list", value };
    }
    return { key, label, kind: "list", value: value.map((entry) => String(entry)) };
  }
  if (isRecord(value)) {
    return { key, label, kind: "object", value };
  }
  return {
    key,
    label,
    kind: "paragraph",
    value: value === null || value === undefined ? "" : String(value),
  };
}

/**
 * Meta fields drawn from the document envelope and payload.meta, when present.
 * Does not include a revision field -- neither the API envelope nor payload.meta
 * carries one today; add it back once the daemon serves it.
 */
export interface DocumentMeta {
  id: string;
  kind: string;
  status: string;
  priority: string | null;
  owner: string;
  date: string | null;
  author: string | null;
}

/** The Badge variant used to render a document's status. */
export type StatusBadgeVariant = "muted" | "success" | "info" | "outline";

export function statusBadgeVariant(status: string): StatusBadgeVariant {
  switch (status) {
    case "draft":
      return "muted";
    case "accepted":
      return "success";
    case "published":
      return "info";
    default:
      return "outline";
  }
}

export function documentMeta(doc: Document, payload: unknown): DocumentMeta {
  const payloadMeta = isRecord(payload) ? payload.meta : null;
  const meta = isRecord(payloadMeta) ? payloadMeta : {};

  return {
    id: doc.id,
    kind: doc.doc_type,
    status: doc.status,
    priority: doc.priority,
    owner: doc.owner,
    date: typeof meta.date === "string" ? meta.date : null,
    author: typeof meta.author === "string" ? meta.author : null,
  };
}
