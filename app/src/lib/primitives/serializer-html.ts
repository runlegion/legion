/**
 * HTML serializer - converts between HTML strings and editor block trees
 *
 * Primary use case: clipboard interop. When users paste from Word, Google Docs,
 * or Pages, the browser clipboard provides text/html. This serializer parses
 * that HTML into blocks. When users copy blocks, it serializes to clean HTML
 * for pasting into other apps.
 *
 * SSR-safe: uses string parsing, not DOMParser or any browser API.
 *
 * @registry-name serializer-html
 * @registry-version 0.1.0
 * @registry-status published
 * @registry-path primitives/serializer-html.ts
 * @registry-type registry:primitive
 *
 * @dependencies none
 * @internal-dependencies primitives/serializer.ts, primitives/types.ts
 */

import type {
  DeserializeResult,
  EditorSerializer,
  SerializerBlock,
} from "@/lib/primitives/serializer";
import { contentToPlainText } from "@/lib/primitives/serializer";
import type { InlineContent, InlineMark } from "@/lib/primitives/types";

// =============================================================================
// ID generation
// =============================================================================

let blockIdCounter = 0;

function nextBlockId(): string {
  blockIdCounter += 1;
  return `html-${blockIdCounter}`;
}

// =============================================================================
// HTML entity handling (no DOM, SSR-safe)
// =============================================================================

const ENTITIES: Record<string, string> = {
  "&amp;": "&",
  "&lt;": "<",
  "&gt;": ">",
  "&quot;": '"',
  "&#39;": "'",
  "&apos;": "'",
  "&nbsp;": " ",
  "&ndash;": "\u2013",
  "&mdash;": "\u2014",
  "&lsquo;": "\u2018",
  "&rsquo;": "\u2019",
  "&ldquo;": "\u201C",
  "&rdquo;": "\u201D",
  "&bull;": "\u2022",
  "&hellip;": "\u2026",
  "&copy;": "\u00A9",
  "&reg;": "\u00AE",
  "&trade;": "\u2122",
};

function decodeEntities(text: string): string {
  return text
    .replace(/&#(\d+);/g, (_, code) => String.fromCharCode(Number(code)))
    .replace(/&#x([0-9a-fA-F]+);/g, (_, code) =>
      String.fromCharCode(Number.parseInt(code as string, 16)),
    )
    .replace(/&\w+;/g, (entity) => ENTITIES[entity] ?? entity);
}

function encodeEntities(text: string): string {
  return text
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;");
}

// =============================================================================
// Minimal HTML tokenizer (SSR-safe, no DOM)
// =============================================================================

interface HtmlToken {
  type: "open" | "close" | "selfclose" | "text";
  tag?: string;
  attrs?: Record<string, string>;
  text?: string;
}

const VOID_ELEMENTS = new Set([
  "br",
  "hr",
  "img",
  "input",
  "meta",
  "link",
  "area",
  "base",
  "col",
  "embed",
  "source",
  "track",
  "wbr",
]);

/** Strip Word/Office markup artifacts from clipboard HTML */
function cleanClipboardHtml(html: string): string {
  return html
    .replace(/<!--\[if[\s\S]*?<!\[endif\]-->/gi, "")
    .replace(/<!--[\s\S]*?-->/g, "")
    .replace(/<style[\s\S]*?<\/style>/gi, "")
    .replace(/\s*mso-[^;"]*;?/gi, "")
    .replace(/<o:p>[\s\S]*?<\/o:p>/gi, "")
    .replace(/<\/?o:\w+[^>]*>/gi, "");
}

function tokenizeHtml(html: string): HtmlToken[] {
  const tokens: HtmlToken[] = [];
  const cleaned = cleanClipboardHtml(html);
  let i = 0;

  while (i < cleaned.length) {
    if (cleaned[i] === "<") {
      const tagEnd = cleaned.indexOf(">", i);
      if (tagEnd === -1) {
        tokens.push({ type: "text", text: cleaned.slice(i) });
        break;
      }

      const tagContent = cleaned.slice(i + 1, tagEnd).trim();

      if (tagContent.startsWith("/")) {
        const tag = tagContent.slice(1).trim().split(/[\s/]/)[0]?.toLowerCase() ?? "";
        tokens.push({ type: "close", tag });
      } else {
        const selfClosing = tagContent.endsWith("/");
        const content = selfClosing ? tagContent.slice(0, -1).trim() : tagContent;
        const spaceIdx = content.search(/[\s/]/);
        const tag = (spaceIdx === -1 ? content : content.slice(0, spaceIdx)).toLowerCase();
        const attrStr = spaceIdx === -1 ? "" : content.slice(spaceIdx).trim();

        const attrs: Record<string, string> = {};
        const attrRegex = /(\w[\w-]*)(?:\s*=\s*(?:"([^"]*)"|'([^']*)'|(\S+)))?/g;
        for (let match = attrRegex.exec(attrStr); match !== null; match = attrRegex.exec(attrStr)) {
          const name = match[1];
          const value = match[2] ?? match[3] ?? match[4] ?? "";
          if (name) attrs[name] = decodeEntities(value);
        }

        tokens.push({
          type: selfClosing || VOID_ELEMENTS.has(tag) ? "selfclose" : "open",
          tag,
          attrs,
        });
      }

      i = tagEnd + 1;
    } else {
      const nextTag = cleaned.indexOf("<", i);
      const text = nextTag === -1 ? cleaned.slice(i) : cleaned.slice(i, nextTag);
      const decoded = decodeEntities(text);
      if (decoded.trim() || decoded.includes("\n")) {
        tokens.push({ type: "text", text: decoded });
      }
      i = nextTag === -1 ? cleaned.length : nextTag;
    }
  }

  return tokens;
}

// =============================================================================
// Deserialize: HTML -> blocks
// =============================================================================

const INLINE_MARK_MAP: Record<string, InlineMark> = {
  strong: "bold",
  b: "bold",
  em: "italic",
  i: "italic",
  code: "code",
  s: "strikethrough",
  del: "strikethrough",
  strike: "strikethrough",
};

function parseInlineTokens(
  tokens: HtmlToken[],
  start: number,
  endTag?: string,
): { content: string | InlineContent[]; end: number } {
  const segments: InlineContent[] = [];

  function addText(text: string, marks: InlineMark[], href?: string): void {
    if (!text) return;
    const segment: InlineContent = { text };
    if (marks.length > 0) segment.marks = [...marks];
    if (href) segment.href = href;
    segments.push(segment);
  }

  function walkInline(idx: number, marks: InlineMark[], href?: string, until?: string): number {
    let j = idx;
    while (j < tokens.length) {
      const token = tokens[j];
      if (!token) break;

      if (token.type === "close" && token.tag === until) return j + 1;

      if (token.type === "text") {
        addText(token.text ?? "", marks, href);
        j += 1;
        continue;
      }

      if (token.type === "open" && token.tag) {
        const mark = INLINE_MARK_MAP[token.tag];
        if (mark) {
          j = walkInline(j + 1, [...marks, mark], href, token.tag);
          continue;
        }
        if (token.tag === "a") {
          j = walkInline(j + 1, [...marks, "link"], token.attrs?.href ?? "", "a");
          continue;
        }
        // Unknown inline tag: consume children
        j = walkInline(j + 1, marks, href, token.tag);
        continue;
      }

      if (token.type === "selfclose") {
        if (token.tag === "br") addText("\n", marks, href);
        j += 1;
        continue;
      }

      // Unexpected close tag
      if (token.type === "close") return j;

      j += 1;
    }
    return j;
  }

  const end = walkInline(start, [], undefined, endTag);

  if (segments.length === 0) return { content: "", end };
  const first = segments[0];
  if (segments.length === 1 && first && !first.marks?.length && !first.href) {
    return { content: first.text, end };
  }
  return { content: segments, end };
}

function deserializeHtml(input: string): DeserializeResult {
  blockIdCounter = 0;
  const tokens = tokenizeHtml(input);
  const blocks: SerializerBlock[] = [];
  let i = 0;

  while (i < tokens.length) {
    const token = tokens[i];
    if (!token) break;

    if (token.type === "text") {
      const trimmed = (token.text ?? "").trim();
      if (trimmed) {
        blocks.push({ id: nextBlockId(), type: "text", content: trimmed });
      }
      i += 1;
      continue;
    }

    if (token.type === "selfclose") {
      if (token.tag === "hr") {
        blocks.push({ id: nextBlockId(), type: "divider" });
      } else if (token.tag === "img") {
        blocks.push({
          id: nextBlockId(),
          type: "image",
          meta: { src: token.attrs?.src ?? "", alt: token.attrs?.alt ?? "" },
        });
      }
      i += 1;
      continue;
    }

    if (token.type === "close") {
      i += 1;
      continue;
    }

    if (token.type === "open" && token.tag) {
      const headingMatch = /^h([1-6])$/.exec(token.tag);
      if (headingMatch) {
        const level = Number(headingMatch[1]);
        const { content, end } = parseInlineTokens(tokens, i + 1, token.tag);
        blocks.push({ id: nextBlockId(), type: "heading", content, meta: { level } });
        i = end;
        continue;
      }

      if (token.tag === "p" || token.tag === "div" || token.tag === "span") {
        const { content, end } = parseInlineTokens(tokens, i + 1, token.tag);
        if (content !== "") {
          blocks.push({ id: nextBlockId(), type: "text", content });
        }
        i = end;
        continue;
      }

      if (token.tag === "pre") {
        let codeContent = "";
        let lang: string | undefined;
        let j = i + 1;
        while (j < tokens.length) {
          const t = tokens[j];
          if (!t) break;
          if (t.type === "close" && t.tag === "pre") {
            j += 1;
            break;
          }
          if (t.type === "open" && t.tag === "code") {
            const cls = t.attrs?.class ?? "";
            const langMatch = /language-(\w+)/.exec(cls);
            if (langMatch) lang = langMatch[1];
            j += 1;
            continue;
          }
          if (t.type === "close" && t.tag === "code") {
            j += 1;
            continue;
          }
          if (t.type === "text") codeContent += t.text ?? "";
          j += 1;
        }
        const codeBlock: SerializerBlock = {
          id: nextBlockId(),
          type: "code",
          content: codeContent,
        };
        if (lang) codeBlock.meta = { language: lang };
        blocks.push(codeBlock);
        i = j;
        continue;
      }

      if (token.tag === "blockquote") {
        const { content, end } = parseInlineTokens(tokens, i + 1, "blockquote");
        blocks.push({ id: nextBlockId(), type: "quote", content });
        i = end;
        continue;
      }

      if (token.tag === "ul" || token.tag === "ol") {
        const ordered = token.tag === "ol";
        const listId = nextBlockId();
        const childIds: string[] = [];
        let j = i + 1;
        while (j < tokens.length) {
          const t = tokens[j];
          if (!t) break;
          if (t.type === "close" && t.tag === token.tag) {
            j += 1;
            break;
          }
          if (t.type === "open" && t.tag === "li") {
            const itemId = nextBlockId();
            childIds.push(itemId);
            const { content, end } = parseInlineTokens(tokens, j + 1, "li");
            blocks.push({ id: itemId, type: "list-item", content, parentId: listId });
            j = end;
            continue;
          }
          j += 1;
        }
        blocks.push({ id: listId, type: "list", meta: { ordered }, children: childIds });
        i = j;
        continue;
      }

      // Unknown block element
      const { content, end } = parseInlineTokens(tokens, i + 1, token.tag);
      if (content !== "") {
        blocks.push({ id: nextBlockId(), type: "text", content });
      }
      i = end;
      continue;
    }

    i += 1;
  }

  return { blocks };
}

// =============================================================================
// Serialize: blocks -> HTML
// =============================================================================

function serializeInlineContent(content: string | InlineContent[] | undefined): string {
  if (content === undefined) return "";
  if (typeof content === "string") return encodeEntities(content);

  return content
    .map((segment) => {
      let html = encodeEntities(segment.text);
      const marks = segment.marks ?? [];

      if (marks.includes("code")) html = `<code>${html}</code>`;
      if (marks.includes("link") && segment.href) {
        html = `<a href="${encodeEntities(segment.href)}">${html}</a>`;
      }
      if (marks.includes("strikethrough")) html = `<del>${html}</del>`;
      if (marks.includes("italic")) html = `<em>${html}</em>`;
      if (marks.includes("bold")) html = `<strong>${html}</strong>`;

      return html;
    })
    .join("");
}

function serializeHtml(blocks: SerializerBlock[]): string {
  const parts: string[] = [];
  const childIds = new Set<string>();
  for (const block of blocks) {
    if (block.children) {
      for (const id of block.children) childIds.add(id);
    }
  }

  for (const block of blocks) {
    if (childIds.has(block.id)) continue;

    switch (block.type) {
      case "heading": {
        const level = Math.min(Math.max((block.meta?.level as number) ?? 1, 1), 6);
        parts.push(`<h${level}>${serializeInlineContent(block.content)}</h${level}>`);
        break;
      }
      case "text":
        parts.push(`<p>${serializeInlineContent(block.content)}</p>`);
        break;
      case "code": {
        const lang = block.meta?.language as string | undefined;
        const cls = lang ? ` class="language-${encodeEntities(lang)}"` : "";
        parts.push(
          `<pre><code${cls}>${encodeEntities(contentToPlainText(block.content))}</code></pre>`,
        );
        break;
      }
      case "quote":
        parts.push(`<blockquote>${serializeInlineContent(block.content)}</blockquote>`);
        break;
      case "divider":
        parts.push("<hr />");
        break;
      case "image": {
        const src = encodeEntities((block.meta?.src as string) ?? "");
        const alt = encodeEntities((block.meta?.alt as string) ?? "");
        parts.push(`<img src="${src}" alt="${alt}" />`);
        break;
      }
      case "list": {
        const tag = block.meta?.ordered ? "ol" : "ul";
        const items = (block.children ?? [])
          .map((childId) => {
            const child = blocks.find((b) => b.id === childId);
            return child ? `<li>${serializeInlineContent(child.content)}</li>` : "";
          })
          .join("");
        parts.push(`<${tag}>${items}</${tag}>`);
        break;
      }
      default:
        if (block.content) {
          parts.push(`<p>${serializeInlineContent(block.content)}</p>`);
        }
    }
  }

  return parts.join("\n");
}

// =============================================================================
// Public API
// =============================================================================

export function createHtmlSerializer(): EditorSerializer {
  return {
    id: "html",
    extensions: [".html", ".htm"],
    deserialize(input: string): DeserializeResult {
      return deserializeHtml(input);
    },
    serialize(blocks: SerializerBlock[]): string {
      return serializeHtml(blocks);
    },
  };
}

/** Pre-built HTML serializer singleton. Stateless per call -- safe to share. */
export const htmlSerializer: EditorSerializer = createHtmlSerializer();
