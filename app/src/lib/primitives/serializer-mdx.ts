/**
 * MDX serializer - converts between MDX strings and editor block trees
 *
 * Wraps the micromark/mdast ecosystem for spec-compliant MDX parsing and
 * serialization. Handles the full MDX v2 format: CommonMark markdown, YAML
 * frontmatter, JSX components with props, JS expressions, and import/export.
 *
 * @registry-name serializer-mdx
 * @registry-version 0.1.0
 * @registry-status published
 * @registry-path primitives/serializer-mdx.ts
 * @registry-type registry:primitive
 *
 * @dependencies micromark, micromark-extension-mdxjs, mdast-util-from-markdown,
 *   mdast-util-to-markdown, mdast-util-mdx
 * @internal-dependencies primitives/serializer.ts, primitives/types.ts
 */
import { fromMarkdown } from "mdast-util-from-markdown";
import { mdxFromMarkdown, mdxToMarkdown } from "mdast-util-mdx";
import { toMarkdown } from "mdast-util-to-markdown";
import { mdxjs } from "micromark-extension-mdxjs";
import type {
  DeserializeResult,
  EditorSerializer,
  SerializerBlock,
} from "@/lib/primitives/serializer";
import type { InlineContent, InlineMark } from "@/lib/primitives/types";

// Hoist extension config objects -- avoids re-allocating on every parse/serialize call
const MDX_SYNTAX = mdxjs();
const MDX_FROM = mdxFromMarkdown();
const MDX_TO = mdxToMarkdown();

// =============================================================================
// Types for mdast nodes (minimal, avoids importing full mdast types)
// =============================================================================

interface MdastNode {
  type: string;
  children?: MdastNode[] | undefined;
  value?: string;
  depth?: number;
  ordered?: boolean;
  spread?: boolean;
  checked?: boolean | null;
  lang?: string | null;
  meta?: string | null;
  url?: string;
  title?: string | null;
  alt?: string;
  name?: string | null;
  attributes?: MdastJsxAttribute[];
  data?: Record<string, unknown>;
}

interface MdastJsxAttribute {
  type: string;
  name?: string;
  value?: string | { type: string; value: string } | null;
}

// =============================================================================
// ID generation
// =============================================================================

let blockIdCounter = 0;

function nextBlockId(): string {
  blockIdCounter += 1;
  return `mdx-${blockIdCounter}`;
}

// =============================================================================
// Frontmatter extraction (pre-parse, since mdast-util-mdx doesn't handle YAML)
// =============================================================================

interface FrontmatterResult {
  frontmatter?: Record<string, unknown>;
  content: string;
}

function extractFrontmatter(input: string): FrontmatterResult {
  if (!input.startsWith("---\n") && !input.startsWith("---\r\n")) {
    return { content: input };
  }

  const endMarker = input.indexOf("\n---", 3);
  if (endMarker === -1) return { content: input };

  const fmRaw = input.slice(4, endMarker);
  const content = input.slice(endMarker + 4).replace(/^\r?\n/, "");

  const frontmatter: Record<string, unknown> = {};
  for (const line of fmRaw.split("\n")) {
    const trimmed = line.trim();
    if (!trimmed || trimmed.startsWith("#")) continue;

    const colonIndex = trimmed.indexOf(":");
    if (colonIndex === -1) continue;

    const key = trimmed.slice(0, colonIndex).trim();
    let value: unknown = trimmed.slice(colonIndex + 1).trim();

    if (value === "true") value = true;
    else if (value === "false") value = false;
    else if (value === "null" || value === "") value = null;
    else if (typeof value === "string" && /^-?\d+$/.test(value)) value = Number.parseInt(value, 10);
    else if (typeof value === "string" && /^-?\d+\.\d+$/.test(value))
      value = Number.parseFloat(value as string);
    else if (typeof value === "string" && /^\[.*\]$/.test(value)) {
      const inner = (value as string).slice(1, -1);
      value = inner
        .split(",")
        .map((s) => s.trim())
        .filter(Boolean)
        .map((s) =>
          (s.startsWith('"') && s.endsWith('"')) || (s.startsWith("'") && s.endsWith("'"))
            ? s.slice(1, -1)
            : s,
        );
    } else if (
      typeof value === "string" &&
      ((value.startsWith('"') && value.endsWith('"')) ||
        (value.startsWith("'") && value.endsWith("'")))
    ) {
      value = (value as string).slice(1, -1);
    }

    if (key) frontmatter[key] = value;
  }

  return { frontmatter, content };
}

function serializeFrontmatter(fm: Record<string, unknown>): string {
  const lines: string[] = [];
  for (const [key, value] of Object.entries(fm)) {
    if (value === null || value === undefined) {
      lines.push(`${key}:`);
    } else if (typeof value === "string") {
      if (/[:#[\]{},]/.test(value)) {
        lines.push(`${key}: "${value.replace(/"/g, '\\"')}"`);
      } else {
        lines.push(`${key}: ${value}`);
      }
    } else if (Array.isArray(value)) {
      const items = value.map((v) => (typeof v === "string" ? `"${v}"` : String(v)));
      lines.push(`${key}: [${items.join(", ")}]`);
    } else {
      lines.push(`${key}: ${String(value)}`);
    }
  }
  return lines.join("\n");
}

// =============================================================================
// mdast -> SerializerBlock conversion
// =============================================================================

function phrasingToInlineContent(children: MdastNode[]): string | InlineContent[] {
  if (!children || children.length === 0) return "";

  const segments: InlineContent[] = [];

  function walk(nodes: MdastNode[], marks: InlineMark[], href?: string): void {
    for (const node of nodes) {
      switch (node.type) {
        case "text":
          if (marks.length > 0 || href) {
            const segment: InlineContent = { text: node.value ?? "" };
            if (marks.length > 0) segment.marks = [...marks];
            if (href) segment.href = href;
            segments.push(segment);
          } else {
            segments.push({ text: node.value ?? "" });
          }
          break;

        case "strong":
          walk(node.children ?? [], [...marks, "bold"], href);
          break;

        case "emphasis":
          walk(node.children ?? [], [...marks, "italic"], href);
          break;

        case "inlineCode":
          segments.push({
            text: node.value ?? "",
            marks: marks.length > 0 ? [...marks, "code"] : ["code"],
          });
          break;

        case "delete":
          walk(node.children ?? [], [...marks, "strikethrough"], href);
          break;

        case "link":
          walk(node.children ?? [], [...marks, "link"], node.url);
          break;

        case "mdxTextExpression":
          segments.push({ text: `{${node.value ?? ""}}` });
          break;

        case "mdxJsxTextElement": {
          // Inline JSX: serialize back to string for now
          const tag = node.name ?? "";
          const attrs = serializeMdastJsxAttributes(node.attributes ?? []);
          if (!node.children?.length) {
            segments.push({ text: `<${tag}${attrs} />` });
          } else {
            // Recurse into children for inline JSX
            segments.push({ text: `<${tag}${attrs}>` });
            walk(node.children, marks, href);
            segments.push({ text: `</${tag}>` });
          }
          break;
        }

        case "break":
          segments.push({ text: "\n" });
          break;

        case "image":
          segments.push({ text: `![${node.alt ?? ""}](${node.url ?? ""})` });
          break;

        default:
          if (node.children) {
            walk(node.children, marks, href);
          } else if (node.value) {
            segments.push({ text: node.value });
          }
      }
    }
  }

  walk(children, []);

  // Simplify: if only one segment with no marks, return plain string
  const onlySegment = segments[0];
  if (segments.length === 1 && onlySegment && !onlySegment.marks?.length && !onlySegment.href) {
    return onlySegment.text;
  }

  // Clean up: remove empty text segments
  const filtered = segments.filter((s) => s.text !== "");
  if (filtered.length === 0) return "";
  const first = filtered[0];
  if (filtered.length === 1 && first && !first.marks?.length && !first.href) {
    return first.text;
  }

  return filtered;
}

function serializeMdastJsxAttributes(attributes: MdastJsxAttribute[]): string {
  if (!attributes.length) return "";
  const parts: string[] = [];
  for (const attr of attributes) {
    if (attr.type === "mdxJsxExpressionAttribute") {
      parts.push(`{${attr.value}}`);
    } else if (attr.name) {
      if (attr.value === null || attr.value === undefined) {
        parts.push(attr.name);
      } else if (typeof attr.value === "string") {
        parts.push(`${attr.name}="${attr.value}"`);
      } else if (
        typeof attr.value === "object" &&
        attr.value.type === "mdxJsxAttributeValueExpression"
      ) {
        parts.push(`${attr.name}={${attr.value.value}}`);
      }
    }
  }
  return parts.length > 0 ? ` ${parts.join(" ")}` : "";
}

function jsxAttributesToProps(attributes: MdastJsxAttribute[]): Record<string, unknown> {
  const props: Record<string, unknown> = {};
  for (const attr of attributes) {
    if (attr.type === "mdxJsxExpressionAttribute") {
      props[`...${attr.value}`] = true;
    } else if (attr.name) {
      if (attr.value === null || attr.value === undefined) {
        props[attr.name] = true;
      } else if (typeof attr.value === "string") {
        props[attr.name] = attr.value;
      } else if (
        typeof attr.value === "object" &&
        attr.value.type === "mdxJsxAttributeValueExpression"
      ) {
        props[attr.name] = `{${attr.value.value}}`;
      }
    }
  }
  return props;
}

function mdastToBlocks(tree: MdastNode): SerializerBlock[] {
  const blocks: SerializerBlock[] = [];
  const esmLines: string[] = [];

  function processNode(node: MdastNode): void {
    switch (node.type) {
      case "root":
        for (const child of node.children ?? []) {
          processNode(child);
        }
        break;

      case "heading":
        blocks.push({
          id: nextBlockId(),
          type: "heading",
          content: phrasingToInlineContent(node.children ?? []),
          meta: { level: node.depth ?? 1 },
        });
        break;

      case "paragraph": {
        // Detect paragraph that wraps a single image (mdast wraps images in paragraphs)
        const pChildren = node.children ?? [];
        const firstChild = pChildren[0];
        if (pChildren.length === 1 && firstChild && firstChild.type === "image") {
          const meta: Record<string, unknown> = {
            src: firstChild.url ?? "",
            alt: firstChild.alt ?? "",
          };
          if (firstChild.title) meta.title = firstChild.title;
          blocks.push({ id: nextBlockId(), type: "image", meta });
          break;
        }
        blocks.push({
          id: nextBlockId(),
          type: "text",
          content: phrasingToInlineContent(pChildren),
        });
        break;
      }

      case "code":
        blocks.push({
          id: nextBlockId(),
          type: "code",
          content: node.value ?? "",
          meta: {
            ...(node.lang ? { language: node.lang } : {}),
            ...(node.meta ? { meta: node.meta } : {}),
          },
        });
        break;

      case "blockquote": {
        // Flatten blockquote children into text content
        const quoteBlocks = mdastToBlocks({
          type: "root",
          children: node.children ?? [],
        });
        // For simple blockquotes with one paragraph, flatten to content
        const firstQuoteBlock = quoteBlocks[0];
        if (quoteBlocks.length === 1 && firstQuoteBlock && firstQuoteBlock.type === "text") {
          const quoteResult: SerializerBlock = {
            id: nextBlockId(),
            type: "quote",
          };
          if (firstQuoteBlock.content !== undefined) {
            quoteResult.content = firstQuoteBlock.content;
          }
          blocks.push(quoteResult);
        } else {
          // Complex blockquotes: store children
          const childIds = quoteBlocks.map((b) => b.id);
          const quoteId = nextBlockId();
          for (const child of quoteBlocks) {
            child.parentId = quoteId;
            blocks.push(child);
          }
          blocks.push({
            id: quoteId,
            type: "quote",
            children: childIds,
          });
        }
        break;
      }

      case "list": {
        const listId = nextBlockId();
        const childIds: string[] = [];
        for (const item of node.children ?? []) {
          if (item.type === "listItem") {
            const itemId = nextBlockId();
            childIds.push(itemId);
            // Flatten simple list items to their paragraph content
            const itemChildren = item.children ?? [];
            const firstItemChild = itemChildren[0];
            const itemMeta =
              item.checked !== null && item.checked !== undefined
                ? { checked: item.checked }
                : undefined;

            if (
              itemChildren.length === 1 &&
              firstItemChild &&
              firstItemChild.type === "paragraph"
            ) {
              const listItemBlock: SerializerBlock = {
                id: itemId,
                type: "list-item",
                content: phrasingToInlineContent(firstItemChild.children ?? []),
                parentId: listId,
              };
              if (itemMeta) listItemBlock.meta = itemMeta;
              blocks.push(listItemBlock);
            } else {
              // Complex list items with multiple blocks
              const innerBlocks = mdastToBlocks({
                type: "root",
                children: itemChildren,
              });
              const innerIds = innerBlocks.map((b) => b.id);
              for (const inner of innerBlocks) {
                inner.parentId = itemId;
                blocks.push(inner);
              }
              const complexItemBlock: SerializerBlock = {
                id: itemId,
                type: "list-item",
                children: innerIds,
                parentId: listId,
              };
              if (itemMeta) complexItemBlock.meta = itemMeta;
              blocks.push(complexItemBlock);
            }
          }
        }
        blocks.push({
          id: listId,
          type: "list",
          meta: { ordered: node.ordered === true },
          children: childIds,
        });
        break;
      }

      case "thematicBreak":
        blocks.push({ id: nextBlockId(), type: "divider" });
        break;

      case "image":
        blocks.push({
          id: nextBlockId(),
          type: "image",
          meta: {
            src: node.url ?? "",
            alt: node.alt ?? "",
            ...(node.title ? { title: node.title } : {}),
          },
        });
        break;

      case "mdxjsEsm":
        esmLines.push(node.value ?? "");
        break;

      case "mdxFlowExpression":
        blocks.push({
          id: nextBlockId(),
          type: "expression",
          content: node.value ?? "",
        });
        break;

      case "mdxJsxFlowElement": {
        const componentName = node.name;
        const props = jsxAttributesToProps(node.attributes ?? []);
        const hasChildren = (node.children?.length ?? 0) > 0;

        if (!hasChildren) {
          blocks.push({
            id: nextBlockId(),
            type: "component",
            meta: {
              component: componentName,
              props,
              selfClosing: true,
            },
          });
        } else {
          // Recursively process children
          const childBlocks = mdastToBlocks({
            type: "root",
            children: node.children ?? [],
          });
          const childIds = childBlocks.map((b) => b.id);
          const componentId = nextBlockId();
          for (const child of childBlocks) {
            child.parentId = componentId;
            blocks.push(child);
          }
          blocks.push({
            id: componentId,
            type: "component",
            meta: {
              component: componentName,
              props,
              selfClosing: false,
            },
            children: childIds,
          });
        }
        break;
      }

      case "html":
        // Raw HTML in MDX context -- store as-is
        blocks.push({
          id: nextBlockId(),
          type: "html",
          content: node.value ?? "",
        });
        break;

      default:
        // Unknown node: try to process children
        if (node.children) {
          for (const child of node.children) {
            processNode(child);
          }
        }
    }
  }

  processNode(tree);

  // Store ESM imports as a special block at the front
  if (esmLines.length > 0) {
    blocks.unshift({
      id: nextBlockId(),
      type: "esm",
      content: esmLines.join("\n"),
    });
  }

  return blocks;
}

// =============================================================================
// SerializerBlock -> mdast conversion
// =============================================================================

function inlineContentToMdast(content: string | InlineContent[] | undefined): MdastNode[] {
  if (content === undefined || content === "") return [{ type: "text", value: "" }];
  if (typeof content === "string") return [{ type: "text", value: content }];

  const nodes: MdastNode[] = [];

  for (const segment of content) {
    const marks = segment.marks ?? [];
    let node: MdastNode = { type: "text", value: segment.text };

    // Wrap in mark nodes from inside out
    if (marks.includes("code")) {
      node = { type: "inlineCode", value: segment.text };
    } else {
      if (marks.includes("link") && segment.href) {
        node = { type: "link", url: segment.href, children: [node] };
      }
      if (marks.includes("strikethrough")) {
        node = { type: "delete", children: [node] };
      }
      if (marks.includes("italic")) {
        node = { type: "emphasis", children: [node] };
      }
      if (marks.includes("bold")) {
        node = { type: "strong", children: [node] };
      }
    }

    nodes.push(node);
  }

  return nodes;
}

function blocksToMdast(
  blocks: SerializerBlock[],
  frontmatter?: Record<string, unknown>,
): MdastNode {
  const children: MdastNode[] = [];

  // Build lookup for parent-child resolution
  const blockMap = new Map<string, SerializerBlock>();
  const childIds = new Set<string>();
  for (const block of blocks) {
    blockMap.set(block.id, block);
    if (block.children) {
      for (const childId of block.children) {
        childIds.add(childId);
      }
    }
  }

  // ESM imports at the top
  const fm = frontmatter ? { ...frontmatter } : undefined;
  let esmImports: string | undefined;
  if (fm?._imports) {
    esmImports = Array.isArray(fm._imports) ? fm._imports.join("\n") : String(fm._imports);
    delete fm._imports;
  }

  // Also check for esm blocks
  for (const block of blocks) {
    if (block.type === "esm") {
      children.push({
        type: "mdxjsEsm",
        value: typeof block.content === "string" ? block.content : "",
      });
    }
  }

  if (esmImports) {
    children.push({ type: "mdxjsEsm", value: esmImports });
  }

  for (const block of blocks) {
    // Skip children (they're rendered by their parent) and ESM blocks (already handled)
    if (childIds.has(block.id) || block.type === "esm") continue;

    const node = blockToMdastNode(block, blockMap);
    if (node) children.push(node);
  }

  return { type: "root", children };
}

function blockToMdastNode(
  block: SerializerBlock,
  blockMap: Map<string, SerializerBlock>,
): MdastNode | null {
  switch (block.type) {
    case "heading":
      return {
        type: "heading",
        depth: (block.meta?.level as number) ?? 1,
        children: inlineContentToMdast(block.content),
      };

    case "text":
      return {
        type: "paragraph",
        children: inlineContentToMdast(block.content),
      };

    case "code":
      return {
        type: "code",
        lang: (block.meta?.language as string) ?? null,
        meta: (block.meta?.meta as string) ?? null,
        value: typeof block.content === "string" ? block.content : "",
      };

    case "quote": {
      if (block.children?.length) {
        // Complex blockquote with children
        const quoteChildren: MdastNode[] = [];
        for (const childId of block.children) {
          const child = blockMap.get(childId);
          if (child) {
            const childNode = blockToMdastNode(child, blockMap);
            if (childNode) quoteChildren.push(childNode);
          }
        }
        return { type: "blockquote", children: quoteChildren };
      }
      return {
        type: "blockquote",
        children: [{ type: "paragraph", children: inlineContentToMdast(block.content) }],
      };
    }

    case "list": {
      const ordered = block.meta?.ordered === true;
      const listItems: MdastNode[] = [];
      for (const childId of block.children ?? []) {
        const child = blockMap.get(childId);
        if (!child) continue;

        if (child.children?.length) {
          // Complex list item
          const itemChildren: MdastNode[] = [];
          for (const innerId of child.children) {
            const inner = blockMap.get(innerId);
            if (inner) {
              const innerNode = blockToMdastNode(inner, blockMap);
              if (innerNode) itemChildren.push(innerNode);
            }
          }
          listItems.push({
            type: "listItem",
            spread: false,
            checked: (child.meta?.checked as boolean) ?? null,
            children: itemChildren,
          });
        } else {
          listItems.push({
            type: "listItem",
            spread: false,
            checked: (child.meta?.checked as boolean) ?? null,
            children: [{ type: "paragraph", children: inlineContentToMdast(child.content) }],
          });
        }
      }
      return { type: "list", ordered, spread: false, children: listItems };
    }

    case "divider":
      return { type: "thematicBreak" };

    case "image":
      return {
        type: "image",
        url: (block.meta?.src as string) ?? "",
        alt: (block.meta?.alt as string) ?? "",
        title: (block.meta?.title as string) ?? null,
      };

    case "component": {
      const componentName = (block.meta?.component as string) ?? null;
      const props = (block.meta?.props as Record<string, unknown>) ?? {};
      const selfClosing = block.meta?.selfClosing !== false && !block.children?.length;

      const attributes: MdastJsxAttribute[] = [];
      for (const [key, value] of Object.entries(props)) {
        if (key.startsWith("...")) {
          attributes.push({ type: "mdxJsxExpressionAttribute", value: key });
        } else if (value === true) {
          attributes.push({ type: "mdxJsxAttribute", name: key, value: null });
        } else if (typeof value === "string" && value.startsWith("{") && value.endsWith("}")) {
          attributes.push({
            type: "mdxJsxAttribute",
            name: key,
            value: { type: "mdxJsxAttributeValueExpression", value: value.slice(1, -1) },
          });
        } else if (typeof value === "string") {
          attributes.push({ type: "mdxJsxAttribute", name: key, value });
        } else if (typeof value === "number" || typeof value === "boolean") {
          attributes.push({
            type: "mdxJsxAttribute",
            name: key,
            value: { type: "mdxJsxAttributeValueExpression", value: String(value) },
          });
        }
      }

      if (selfClosing) {
        return {
          type: "mdxJsxFlowElement",
          name: componentName,
          attributes,
          children: [],
        };
      }

      // Component with children
      const componentChildren: MdastNode[] = [];
      for (const childId of block.children ?? []) {
        const child = blockMap.get(childId);
        if (child) {
          const childNode = blockToMdastNode(child, blockMap);
          if (childNode) componentChildren.push(childNode);
        }
      }
      return {
        type: "mdxJsxFlowElement",
        name: componentName,
        attributes,
        children: componentChildren,
      };
    }

    case "expression":
      return {
        type: "mdxFlowExpression",
        value: typeof block.content === "string" ? block.content : "",
      };

    case "html":
      return {
        type: "html",
        value: typeof block.content === "string" ? block.content : "",
      };

    default:
      // Unknown type: render as paragraph
      if (block.content) {
        return {
          type: "paragraph",
          children: inlineContentToMdast(block.content),
        };
      }
      return null;
  }
}

// =============================================================================
// Public API
// =============================================================================

/**
 * Create an MDX serializer for the editor block format.
 *
 * Uses the micromark/mdast ecosystem for spec-compliant MDX v2 parsing.
 * Handles: CommonMark markdown, JSX components with props, JS expressions,
 * import/export statements, and YAML frontmatter.
 *
 * @example
 * ```ts
 * const mdx = createMdxSerializer();
 * const { blocks, frontmatter } = mdx.deserialize(mdxString);
 * const output = mdx.serialize(blocks, frontmatter);
 * ```
 */
export function createMdxSerializer(): EditorSerializer {
  return {
    id: "mdx",
    extensions: [".mdx", ".md"],

    deserialize(input: string): DeserializeResult {
      blockIdCounter = 0;

      // Extract frontmatter before mdast parsing
      const { frontmatter, content } = extractFrontmatter(input);

      // Parse with micromark + MDX extensions
      const tree = fromMarkdown(content, {
        extensions: [MDX_SYNTAX],
        mdastExtensions: [MDX_FROM],
      });

      const blocks = mdastToBlocks(tree as MdastNode);

      const result: DeserializeResult = { blocks };
      if (frontmatter && Object.keys(frontmatter).length > 0) {
        result.frontmatter = frontmatter;
      }
      return result;
    },

    serialize(blocks: SerializerBlock[], frontmatter?: Record<string, unknown>): string {
      const fm = frontmatter ? { ...frontmatter } : undefined;

      // Build mdast tree from blocks
      const tree = blocksToMdast(blocks, fm);

      // Serialize with mdast-util-to-markdown + MDX extensions
      let output = toMarkdown(tree as Parameters<typeof toMarkdown>[0], {
        extensions: [MDX_TO],
        rule: "-",
      });

      // Prepend frontmatter
      const cleanFm = fm ? { ...fm } : undefined;
      if (cleanFm) delete cleanFm._imports;
      if (cleanFm && Object.keys(cleanFm).length > 0) {
        output = `---\n${serializeFrontmatter(cleanFm)}\n---\n\n${output}`;
      }

      return output;
    },
  };
}

/** Pre-built MDX serializer singleton. Uses a shared block ID counter -- safe for sequential use. */
export const mdxSerializer: EditorSerializer = createMdxSerializer();
