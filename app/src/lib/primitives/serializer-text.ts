/**
 * Plain text serializer - converts between plain text and editor block trees
 *
 * Fallback format for accessibility, clipboard paste without HTML, search
 * indexing, and simple text extraction. Explicitly lossy: inline marks are
 * stripped on serialize and not recovered on deserialize.
 *
 * @registry-name serializer-text
 * @registry-version 0.1.0
 * @registry-status published
 * @registry-path primitives/serializer-text.ts
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

let blockIdCounter = 0;

function nextBlockId(): string {
  blockIdCounter += 1;
  return `text-${blockIdCounter}`;
}

function deserializeText(input: string): DeserializeResult {
  blockIdCounter = 0;
  const blocks: SerializerBlock[] = [];

  if (!input.trim()) return { blocks };

  const paragraphs = input.split(/\n{2,}/);

  for (const para of paragraphs) {
    const trimmed = para.trim();
    if (!trimmed) continue;

    if (/^[-*_]{3,}$/.test(trimmed)) {
      blocks.push({ id: nextBlockId(), type: "divider" });
      continue;
    }

    const headingMatch = /^(#{1,4})\s+(.+)$/.exec(trimmed);
    if (headingMatch) {
      const hashes = headingMatch[1] ?? "#";
      const text = headingMatch[2] ?? "";
      blocks.push({
        id: nextBlockId(),
        type: "heading",
        content: text,
        meta: { level: hashes.length },
      });
      continue;
    }

    const lines = trimmed.split("\n");
    const firstLine = lines[0]?.trim() ?? "";

    if (/^[-*]\s+/.test(firstLine) && lines.every((l) => /^\s*[-*]\s+/.test(l) || !l.trim())) {
      const listId = nextBlockId();
      const childIds: string[] = [];
      for (const line of lines) {
        const itemText = line.trim().replace(/^[-*]\s+/, "");
        if (!itemText) continue;
        const itemId = nextBlockId();
        childIds.push(itemId);
        blocks.push({ id: itemId, type: "list-item", content: itemText, parentId: listId });
      }
      blocks.push({ id: listId, type: "list", meta: { ordered: false }, children: childIds });
      continue;
    }

    if (/^\d+\.\s+/.test(firstLine) && lines.every((l) => /^\s*\d+\.\s+/.test(l) || !l.trim())) {
      const listId = nextBlockId();
      const childIds: string[] = [];
      for (const line of lines) {
        const itemText = line.trim().replace(/^\d+\.\s+/, "");
        if (!itemText) continue;
        const itemId = nextBlockId();
        childIds.push(itemId);
        blocks.push({ id: itemId, type: "list-item", content: itemText, parentId: listId });
      }
      blocks.push({ id: listId, type: "list", meta: { ordered: true }, children: childIds });
      continue;
    }

    blocks.push({ id: nextBlockId(), type: "text", content: trimmed });
  }

  return { blocks };
}

function serializeText(blocks: SerializerBlock[]): string {
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
        const level = (block.meta?.level as number) ?? 1;
        const prefix = "#".repeat(Math.min(Math.max(level, 1), 4));
        parts.push(`${prefix} ${contentToPlainText(block.content)}`);
        break;
      }
      case "text":
      case "code":
        parts.push(contentToPlainText(block.content));
        break;
      case "quote":
        parts.push(
          contentToPlainText(block.content)
            .split("\n")
            .map((line) => `> ${line}`)
            .join("\n"),
        );
        break;
      case "divider":
        parts.push("---");
        break;
      case "image":
        parts.push((block.meta?.alt as string) ?? "");
        break;
      case "list": {
        const ordered = block.meta?.ordered === true;
        const items = (block.children ?? [])
          .map((childId, idx) => {
            const child = blocks.find((b) => b.id === childId);
            if (!child) return "";
            const prefix = ordered ? `${idx + 1}. ` : "- ";
            return `${prefix}${contentToPlainText(child.content)}`;
          })
          .filter(Boolean);
        parts.push(items.join("\n"));
        break;
      }
      default:
        if (block.content) parts.push(contentToPlainText(block.content));
    }
  }

  return `${parts.join("\n\n")}\n`;
}

export function createTextSerializer(): EditorSerializer {
  return {
    id: "text",
    extensions: [".txt"],
    deserialize(input: string): DeserializeResult {
      return deserializeText(input);
    },
    serialize(blocks: SerializerBlock[]): string {
      return serializeText(blocks);
    },
  };
}

/** Pre-built text serializer singleton. Stateless per call -- safe to share. */
export const textSerializer: EditorSerializer = createTextSerializer();
