/**
 * Clipboard primitive for copy/cut/paste operations
 * Client-only, returns cleanup function
 * SSR-safe: checks for window existence
 */

import type { CleanupFunction } from "@/lib/primitives/types";

/**
 * Data structure for clipboard content
 */
export interface ClipboardData {
  text?: string;
  html?: string;
  custom?: unknown;
}

/**
 * Options for creating a clipboard manager
 */
export interface ClipboardOptions {
  container: HTMLElement;
  customMimeType?: string;
  onPaste?: (data: ClipboardData) => void;
  onCopy?: (data: ClipboardData) => void;
  onCut?: (data: ClipboardData) => void;
}

/**
 * Return type for createClipboard
 */
export interface ClipboardManager {
  write: (data: ClipboardData) => Promise<void>;
  read: () => Promise<ClipboardData>;
  cleanup: CleanupFunction;
}

/**
 * Parse clipboard data from a ClipboardEvent or Navigator Clipboard API
 */
async function parseClipboardData(
  clipboardData: DataTransfer | null,
  customMimeType?: string,
): Promise<ClipboardData> {
  const result: ClipboardData = {};

  if (!clipboardData) {
    return result;
  }

  // Get text content
  const text = clipboardData.getData("text/plain");
  if (text) {
    result.text = text;
  }

  // Get HTML content
  const html = clipboardData.getData("text/html");
  if (html) {
    result.html = html;
  }

  // Get custom content if mime type is specified
  if (customMimeType) {
    const customData = clipboardData.getData(customMimeType);
    if (customData) {
      try {
        result.custom = JSON.parse(customData);
      } catch {
        // If parsing fails, store as string
        result.custom = customData;
      }
    }
  }

  return result;
}

/**
 * Create a clipboard manager for an element
 * Provides read/write operations and event callbacks for copy/cut/paste
 *
 * @example
 * const { write, read, cleanup } = createClipboard({
 *   container: editorElement,
 *   customMimeType: 'application/x-rafters-blocks',
 *   onPaste: (data) => console.log('Pasted:', data),
 *   onCopy: (data) => console.log('Copied:', data),
 *   onCut: (data) => console.log('Cut:', data),
 * });
 *
 * // Write to clipboard
 * await write({ text: 'Hello', custom: { blocks: [...] } });
 *
 * // Read from clipboard
 * const data = await read();
 *
 * // Cleanup when done
 * cleanup();
 */
export function createClipboard(options: ClipboardOptions): ClipboardManager {
  const { container, customMimeType, onPaste, onCopy, onCut } = options;

  // SSR guard
  if (typeof window === "undefined") {
    return {
      write: async () => {},
      read: async () => ({}),
      cleanup: () => {},
    };
  }

  const handlePaste = async (event: ClipboardEvent) => {
    if (!onPaste) return;

    const data = await parseClipboardData(event.clipboardData, customMimeType);
    onPaste(data);
  };

  const handleCopy = async (event: ClipboardEvent) => {
    if (!onCopy) return;

    const data = await parseClipboardData(event.clipboardData, customMimeType);
    onCopy(data);
  };

  const handleCut = async (event: ClipboardEvent) => {
    if (!onCut) return;

    const data = await parseClipboardData(event.clipboardData, customMimeType);
    onCut(data);
  };

  // Add event listeners
  container.addEventListener("paste", handlePaste);
  container.addEventListener("copy", handleCopy);
  container.addEventListener("cut", handleCut);

  /**
   * Write data to the clipboard
   * Uses the modern Clipboard API with fallback for custom MIME types
   */
  const write = async (data: ClipboardData): Promise<void> => {
    try {
      // Build clipboard items
      const items: Record<string, Blob> = {};

      if (data.text) {
        items["text/plain"] = new Blob([data.text], { type: "text/plain" });
      }

      if (data.html) {
        items["text/html"] = new Blob([data.html], { type: "text/html" });
      }

      if (data.custom !== undefined && customMimeType) {
        const customString =
          typeof data.custom === "string" ? data.custom : JSON.stringify(data.custom);
        items[customMimeType] = new Blob([customString], { type: customMimeType });
      }

      // If we have any items, write them to clipboard
      if (Object.keys(items).length > 0) {
        try {
          // Try to write with all MIME types
          const clipboardItem = new ClipboardItem(items);
          await navigator.clipboard.write([clipboardItem]);
        } catch {
          // Custom MIME types may not be supported, fallback to text only
          if (data.text) {
            await navigator.clipboard.writeText(data.text);
          }
        }
      }
    } catch {
      // Handle permission denied or other errors gracefully
      // Do not throw - caller can check clipboard state if needed
    }
  };

  /**
   * Read data from the clipboard
   * Returns empty ClipboardData if clipboard is empty or permission denied
   */
  const read = async (): Promise<ClipboardData> => {
    const result: ClipboardData = {};

    try {
      // Try to read all clipboard items
      const items = await navigator.clipboard.read();

      for (const item of items) {
        // Read text/plain
        if (item.types.includes("text/plain")) {
          try {
            const blob = await item.getType("text/plain");
            result.text = await blob.text();
          } catch {
            // Type not available, skip
          }
        }

        // Read text/html
        if (item.types.includes("text/html")) {
          try {
            const blob = await item.getType("text/html");
            result.html = await blob.text();
          } catch {
            // Type not available, skip
          }
        }

        // Read custom MIME type
        if (customMimeType && item.types.includes(customMimeType)) {
          try {
            const blob = await item.getType(customMimeType);
            const customString = await blob.text();
            try {
              result.custom = JSON.parse(customString);
            } catch {
              result.custom = customString;
            }
          } catch {
            // Type not available, skip
          }
        }
      }
    } catch {
      // Permission denied or clipboard empty - return empty result
      // Try fallback to readText for basic text content
      try {
        const text = await navigator.clipboard.readText();
        if (text) {
          result.text = text;
        }
      } catch {
        // Still failed, return empty result
      }
    }

    return result;
  };

  const cleanup: CleanupFunction = () => {
    container.removeEventListener("paste", handlePaste);
    container.removeEventListener("copy", handleCopy);
    container.removeEventListener("cut", handleCut);
  };

  return {
    write,
    read,
    cleanup,
  };
}
