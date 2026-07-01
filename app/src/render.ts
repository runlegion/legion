// Document rendering. Two shapes, decided by payload:
//   - prose docs carry a `markdown`/`body` field -> render the text.
//   - spec/design docs carry structured typed JSON -> render the STRUCTURE
//     (fields, badges, acceptance list, linked depends_on/traces_to), not a
//     flattened prose dump. Sean: "a drop of the blog does me little good."
import type { DocumentRow } from "./api.ts";

/// The three sidebar sections. Kanban is handled separately (it is a board,
/// not a document list).
export const SERVICE_DESIGN_TYPES = [
  "persona",
  "journey",
  "blueprint",
  "ecosystem",
  "painmatrix",
];
export const SPEC_TYPES = ["requirement", "nfr", "schema"];
const PROSE_KEYS = ["markdown", "body", "content"];

export function sectionForType(docType: string): "design" | "spec" | "other" {
  if (SERVICE_DESIGN_TYPES.includes(docType)) return "design";
  if (SPEC_TYPES.includes(docType)) return "spec";
  return "other";
}

export function escapeHtml(s: string): string {
  return s
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;");
}

/// Deliberately tiny markdown pass for prose bodies: headings, bold, inline
/// code, and paragraphs. Not a full MDX engine -- prose docs are the
/// secondary surface here; the structured renderer is the point.
function renderMarkdown(md: string): string {
  const body = md.replace(/^---\n[\s\S]*?\n---\n/, ""); // strip frontmatter
  return body
    .split(/\n{2,}/)
    .map((block) => {
      const h = block.match(/^(#{1,4})\s+(.*)$/);
      if (h) {
        const level = h[1].length;
        return `<h${level}>${inline(h[2])}</h${level}>`;
      }
      return `<p>${inline(block).replace(/\n/g, "<br>")}</p>`;
    })
    .join("\n");
}

function inline(s: string): string {
  return escapeHtml(s)
    .replace(/`([^`]+)`/g, "<code>$1</code>")
    .replace(/\*\*([^*]+)\*\*/g, "<strong>$1</strong>");
}

function badge(label: string, value: string): string {
  return `<span class="badge" data-k="${escapeHtml(label)}">${escapeHtml(value)}</span>`;
}

function refLink(id: string): string {
  return `<a class="ref" data-doc="${escapeHtml(id)}" href="#${escapeHtml(id)}">${escapeHtml(id)}</a>`;
}

function refs(value: unknown): string {
  const arr = Array.isArray(value) ? value : [value];
  return arr
    .filter((v) => typeof v === "string" && v)
    .map((v) => refLink(v as string))
    .join(" ");
}

/// Render one document as HTML. Chooses prose vs structured by payload shape.
export function renderDocument(row: DocumentRow): string {
  let payload: Record<string, unknown> = {};
  try {
    payload = JSON.parse(row.payload) as Record<string, unknown>;
  } catch {
    return `<pre class="raw">${escapeHtml(row.payload)}</pre>`;
  }

  const proseKey = PROSE_KEYS.find((k) => typeof payload[k] === "string");
  if (proseKey) {
    return `<article class="prose">${renderMarkdown(payload[proseKey] as string)}</article>`;
  }
  return renderStructured(row, payload);
}

function renderStructured(
  row: DocumentRow,
  payload: Record<string, unknown>,
): string {
  const meta = (payload.meta as Record<string, unknown>) ?? {};
  const title = (payload.title as string) ?? row.id;
  const description = payload.description as string | undefined;

  const badges: string[] = [];
  const status = (meta.status as string) ?? row.status;
  badges.push(badge("status", status));
  const priority = (meta.priority as string) ?? row.priority ?? undefined;
  if (priority) badges.push(badge("priority", priority));
  if (meta.phase !== undefined) badges.push(badge("phase", String(meta.phase)));
  const owner = (meta.owner as string) ?? row.owner;
  if (owner) badges.push(badge("owner", owner));

  const parts: string[] = [];
  parts.push(`<div class="doc-badges">${badges.join("")}</div>`);
  parts.push(`<h2 class="doc-title">${escapeHtml(title)}</h2>`);
  if (description) parts.push(`<p class="doc-desc">${inline(description)}</p>`);

  if (payload.traces_to) {
    parts.push(fieldBlock("Traces to", refs(payload.traces_to)));
  }
  if (payload.depends_on) {
    parts.push(fieldBlock("Depends on", refs(payload.depends_on)));
  }

  const acceptance = ((payload.verification as Record<string, unknown>)
    ?.acceptance ?? payload.acceptance) as unknown;
  if (Array.isArray(acceptance) && acceptance.length) {
    const items = acceptance
      .map((a) => `<li>${inline(String(a))}</li>`)
      .join("");
    parts.push(`<div class="field"><h3>Acceptance</h3><ul class="accept">${items}</ul></div>`);
  }

  // Anything structured we did not special-case, shown so nothing is hidden.
  const shown = new Set([
    "meta",
    "title",
    "description",
    "traces_to",
    "depends_on",
    "verification",
    "acceptance",
  ]);
  const extras = Object.entries(payload).filter(([k]) => !shown.has(k));
  if (extras.length) {
    const rows = extras
      .map(
        ([k, v]) =>
          `<div class="kv"><dt>${escapeHtml(k)}</dt><dd>${escapeHtml(
            typeof v === "string" ? v : JSON.stringify(v),
          )}</dd></div>`,
      )
      .join("");
    parts.push(`<div class="field"><h3>Other</h3><dl class="extras">${rows}</dl></div>`);
  }

  return `<div class="doc-structured">${parts.join("\n")}</div>`;
}

function fieldBlock(label: string, html: string): string {
  return `<div class="field inline"><h3>${escapeHtml(label)}</h3><div>${html}</div></div>`;
}
