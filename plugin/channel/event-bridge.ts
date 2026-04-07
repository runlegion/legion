import type { Server } from "@modelcontextprotocol/sdk/server/index.js";
import type { LegionEvent } from "./types.js";

// Maximum content length for channel notifications.
// Claude Code truncates content beyond this limit silently.
const MAX_NOTIFICATION_LENGTH = 2000;

// Truncate long content with a pointer to the bullpen.
// Reserves space for the hint so the total stays under the limit.
function truncateIfNeeded(content: string, id?: string): string {
  if (content.length <= MAX_NOTIFICATION_LENGTH) return content;
  const hint = id
    ? `\n\n[truncated -- full content on bullpen, id: ${id}]`
    : "\n\n[truncated -- full content on bullpen]";
  return content.slice(0, Math.max(0, MAX_NOTIFICATION_LENGTH - hint.length)) + hint;
}

// Format an event into a human-readable channel message.
function formatEvent(event: LegionEvent): string {
  switch (event.type) {
    case "post":
      return truncateIfNeeded(`[post] ${event.from}: ${event.text}`, event.id);
    case "signal":
      return truncateIfNeeded(
        `[signal] @${event.to} ${event.verb}${event.status ? ":" + event.status : ""} from ${event.from}${event.note ? " -- " + event.note : ""}`,
        event.id
      );
    case "task": {
      const prio =
        event.priority !== "med" ? ` [${event.priority}]` : "";
      return truncateIfNeeded(
        `[task${prio}] ${event.from} assigned: "${event.text}"${event.context ? " (context: " + event.context + ")" : ""}`,
        event.id
      );
    }
    case "discord":
      return truncateIfNeeded(
        `[discord #${event.channel}] ${event.author}: ${event.text}`
      );
  }
}

// Build meta attributes for the channel notification.
function buildMeta(
  event: LegionEvent
): Record<string, string> {
  const meta: Record<string, string> = { type: event.type };

  switch (event.type) {
    case "post":
      meta.from = event.from;
      meta.id = event.id;
      break;
    case "signal":
      meta.from = event.from;
      meta.to = event.to;
      meta.verb = event.verb;
      if (event.status) meta.status = event.status;
      meta.id = event.id;
      break;
    case "task":
      meta.from = event.from;
      meta.to = event.to;
      meta.id = event.id;
      meta.priority = event.priority;
      break;
    case "discord":
      meta.channel = event.channel;
      meta.author = event.author;
      break;
  }

  return meta;
}

// Bridge a Legion event into a Claude Code channel notification.
export async function bridgeEvent(
  server: Server,
  event: LegionEvent,
  _repo: string
): Promise<void> {
  const content = formatEvent(event);
  const meta = buildMeta(event);

  try {
    await server.notification({
      method: "notifications/claude/channel",
      params: { content, meta },
    });
  } catch (err) {
    console.error("[legion-channel] failed to push notification:", err);
  }
}
