/**
 * Editor serializer primitive - converts between EditorBlock trees and external formats
 *
 * The block tree is the universal intermediate representation. Every external format
 * (JSON, HTML, MDX, plain text) converts to/from blocks through the EditorSerializer
 * interface. The editor never knows what format it is working with.
 *
 * This is a leaf primitive: zero external dependencies, SSR-safe, framework-agnostic.
 *
 * @registry-name serializer
 * @registry-version 0.1.0
 * @registry-status published
 * @registry-path primitives/serializer.ts
 * @registry-type registry:primitive
 *
 * @cognitive-load 3/10 - Simple interface with pure function implementations
 * @attention-economics Single entry point per format; consumers pick a serializer and call two methods
 * @trust-building Round-trip fidelity: deserialize(serialize(blocks)) preserves all content
 * @accessibility Format-agnostic block tree preserves semantic structure across formats
 * @semantic-meaning Serializer = bridge between internal block model and external format
 *
 * @dependencies none
 * @devDependencies
 * @internal-dependencies
 *
 * @usage-patterns
 * DO: Use createJsonSerializer for autosave, clipboard, undo history, and engineer APIs
 * DO: Implement EditorSerializer for custom formats
 * DO: Use the frontmatter field for metadata (title, date, imports, etc.)
 * NEVER: Access DOM or browser APIs in serializer implementations
 * NEVER: Assume a specific block type set -- serializers handle unknown types gracefully
 */
import type { BaseBlock, InlineContent } from "@/lib/primitives/types";

// =============================================================================
// Types
// =============================================================================

/**
 * Block type for serialization. Identical to BaseBlock -- aliased here so
 * serializer consumers import from one module without reaching into types.ts.
 * EditorBlock extends BaseBlock with runtime-only fields (rules) that are
 * intentionally excluded from serialization.
 */
export type SerializerBlock = BaseBlock;

/**
 * Result of deserializing an external format into blocks.
 */
export interface DeserializeResult {
  /** Parsed blocks */
  blocks: SerializerBlock[];
  /** Optional frontmatter/metadata extracted from the format (YAML front matter, JSON meta, etc.) */
  frontmatter?: Record<string, unknown>;
}

/**
 * Contract for converting between EditorBlock trees and external formats.
 *
 * Implementations must be:
 * - SSR-safe (no DOM, no window, no document)
 * - Pure (no side effects, deterministic output for the same input)
 * - Framework-agnostic (vanilla TS only)
 */
export interface EditorSerializer {
  /** Format identifier (e.g. 'json', 'html', 'mdx', 'text') */
  readonly id: string;

  /** File extensions this serializer handles (e.g. ['.json'], ['.mdx', '.md']) */
  readonly extensions: readonly string[];

  /** Parse an external format string into blocks */
  deserialize(input: string): DeserializeResult;

  /** Convert blocks to an external format string */
  serialize(blocks: SerializerBlock[], frontmatter?: Record<string, unknown>): string;
}

// =============================================================================
// Inline content helpers
// =============================================================================

/**
 * Extract plain text from content, stripping all marks.
 */
export function contentToPlainText(content: string | InlineContent[] | undefined): string {
  if (content === undefined) return "";
  if (typeof content === "string") return content;
  return content.map((segment) => segment.text).join("");
}

/**
 * Check whether content has any inline marks.
 */
export function contentHasMarks(content: string | InlineContent[] | undefined): boolean {
  if (content === undefined || typeof content === "string") return false;
  return content.some((segment) => segment.marks && segment.marks.length > 0);
}

// =============================================================================
// JSON Serializer
// =============================================================================

interface JsonSerializerPayload {
  version: 1;
  blocks: SerializerBlock[];
  frontmatter?: Record<string, unknown>;
}

function isJsonPayload(value: unknown): value is JsonSerializerPayload {
  if (typeof value !== "object" || value === null) return false;
  const obj = value as Record<string, unknown>;
  return obj.version === 1 && Array.isArray(obj.blocks);
}

/** Only validates the first element -- assumes trusted input from our own serialize output. */
function isBlockArray(value: unknown): value is SerializerBlock[] {
  return Array.isArray(value) && (value.length === 0 || isSerializerBlock(value[0]));
}

function isSerializerBlock(value: unknown): value is SerializerBlock {
  if (typeof value !== "object" || value === null) return false;
  const obj = value as Record<string, unknown>;
  return typeof obj.id === "string" && typeof obj.type === "string";
}

/**
 * Create a JSON serializer for the editor block format.
 *
 * The JSON format wraps blocks in a versioned envelope:
 * ```json
 * {
 *   "version": 1,
 *   "blocks": [...],
 *   "frontmatter": { ... }
 * }
 * ```
 *
 * Also accepts a bare block array for backwards compatibility and convenience.
 *
 * @example
 * ```ts
 * const json = createJsonSerializer();
 * const output = json.serialize(blocks, { title: 'My page' });
 * const { blocks: parsed, frontmatter } = json.deserialize(output);
 * ```
 */
export function createJsonSerializer(): EditorSerializer {
  return {
    id: "json",
    extensions: [".json"],

    deserialize(input: string): DeserializeResult {
      const parsed: unknown = JSON.parse(input);

      if (isJsonPayload(parsed)) {
        const result: DeserializeResult = { blocks: parsed.blocks };
        if (parsed.frontmatter) {
          result.frontmatter = parsed.frontmatter;
        }
        return result;
      }

      if (isBlockArray(parsed)) {
        return { blocks: parsed };
      }

      throw new Error(
        "Invalid JSON format: expected { version: 1, blocks: [...] } or a block array",
      );
    },

    serialize(blocks: SerializerBlock[], frontmatter?: Record<string, unknown>): string {
      const payload: JsonSerializerPayload = {
        version: 1,
        blocks,
      };
      if (frontmatter && Object.keys(frontmatter).length > 0) {
        payload.frontmatter = frontmatter;
      }
      return JSON.stringify(payload, null, 2);
    },
  };
}

/** Pre-built JSON serializer singleton. Stateless, safe to share across callers. */
export const jsonSerializer: EditorSerializer = createJsonSerializer();
