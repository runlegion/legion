import type { LegacyFeedItem, LegacyTask, LegionEvent } from "./types.js";

interface SSEConfig {
  port: number;
  repo: string;
  onEvent: (event: LegionEvent) => void;
  onConnect: () => void;
}

// Parse a signal text into structured fields.
// Signals start with @recipient and have verb:status format.
function parseSignalText(
  text: string,
  id: string,
  repo: string,
  timestamp: string
): LegionEvent | null {
  // Signal format: @recipient verb:status -- note
  // Or: @recipient verb -- freetext
  const match = text.match(/^@(\S+)\s+([a-zA-Z-]+)(?::(\S+))?\s*(.*)/);
  if (!match) return null;
  // Strip leading "-- " from note if present
  const rawNote = match[4] || "";
  const note = rawNote.replace(/^--\s*/, "");
  return {
    type: "signal",
    id,
    from: repo,
    to: match[1],
    verb: match[2],
    status: match[3] || null,
    note: note || null,
    timestamp,
  };
}

// Convert a legacy feed item to a typed event.
function feedItemToEvent(item: LegacyFeedItem): LegionEvent {
  if (item.is_signal) {
    const signal = parseSignalText(
      item.text,
      item.id,
      item.repo,
      item.created_at
    );
    if (signal) return signal;
  }
  return {
    type: "post",
    id: item.id,
    from: item.repo,
    text: item.text,
    timestamp: item.created_at,
    is_signal: item.is_signal,
  };
}

// Convert a legacy task to a typed event.
function taskToEvent(task: LegacyTask): LegionEvent {
  return {
    type: "task",
    id: task.id,
    from: task.from_repo,
    to: task.to_repo,
    text: task.text,
    priority: task.priority,
    status: task.status,
    context: task.context,
    timestamp: task.updated_at,
  };
}

// Check if a feed item is relevant to this agent on startup.
// Only deliver: signals addressed to this agent, or @all blockers.
function isRelevantBacklog(item: LegacyFeedItem, repo: string): boolean {
  const text = item.text.toLowerCase();
  const atMe = `@${repo.toLowerCase()}`;

  // Direct signal to this agent
  if (text.startsWith(atMe)) return true;

  // @all blocker
  if (text.startsWith("@all") && (text.includes("blocker") || text.includes("blocked"))) {
    return true;
  }

  return false;
}

// Fetch initial backlog from REST API before SSE streaming begins.
// Only delivers direct signals and @all blockers -- everything else is pull-based via bullpen.
async function fetchBacklog(config: SSEConfig): Promise<void> {
  const base = `http://localhost:${config.port}`;

  try {
    const [feedRes, taskRes] = await Promise.all([
      fetch(`${base}/api/feed?filter=signals`),
      fetch(`${base}/api/tasks`),
    ]);

    if (feedRes.ok) {
      const items = (await feedRes.json()) as LegacyFeedItem[];
      for (const item of items) {
        if (item.repo === config.repo) continue;
        if (!isRelevantBacklog(item, config.repo)) continue;
        config.onEvent(feedItemToEvent(item));
      }
    }

    if (taskRes.ok) {
      const tasks = (await taskRes.json()) as LegacyTask[];
      for (const task of tasks) {
        if (task.to_repo !== config.repo || task.status !== "pending") continue;
        config.onEvent(taskToEvent(task));
      }
    }
  } catch {
    console.error("[legion-channel] backlog fetch failed, will rely on SSE");
  }
}

// Parse SSE text stream into event/data pairs.
function parseSSEChunk(chunk: string): Array<{ event: string; data: string }> {
  const events: Array<{ event: string; data: string }> = [];
  let currentEvent = "";
  let currentData = "";

  for (const line of chunk.split("\n")) {
    if (line.startsWith("event:")) {
      currentEvent = line.slice(6).trim();
    } else if (line.startsWith("data:")) {
      // SSE spec: multiple data: lines are concatenated with newlines
      const value = line.slice(5).trim();
      currentData = currentData ? currentData + "\n" + value : value;
    } else if (line === "") {
      if (currentEvent && currentData) {
        events.push({ event: currentEvent, data: currentData });
      }
      currentEvent = "";
      currentData = "";
    }
  }

  return events;
}

export async function connectSSE(config: SSEConfig): Promise<void> {
  const seenIds = new Set<string>();
  let backoff = 1000;
  const maxBackoff = 30000;

  // Fetch initial state before streaming
  await fetchBacklog(config);

  async function connect(): Promise<void> {
    const url = `http://localhost:${config.port}/sse`;

    try {
      const response = await fetch(url);
      if (!response.ok || !response.body) {
        throw new Error(`SSE connection failed: ${response.status}`);
      }

      backoff = 1000; // Reset on successful connection
      config.onConnect();

      const reader = response.body.getReader();
      const decoder = new TextDecoder();
      let buffer = "";

      while (true) {
        const { done, value } = await reader.read();
        if (done) break;

        buffer += decoder.decode(value, { stream: true });

        // Process complete SSE messages (double newline terminated)
        const parts = buffer.split("\n\n");
        buffer = parts.pop() || "";

        for (const part of parts) {
          const sseEvents = parseSSEChunk(part + "\n\n");

          for (const sse of sseEvents) {
            if (sse.event === "ping") continue;

            try {
              const parsed = JSON.parse(sse.data);

              if (sse.event === "feed") {
                // Feed events are arrays of items -- only deliver @me and @all blockers
                const items = parsed as LegacyFeedItem[];
                for (const item of items) {
                  if (item.repo === config.repo) continue;
                  if (seenIds.has(item.id)) continue;
                  if (!isRelevantBacklog(item, config.repo)) continue;
                  seenIds.add(item.id);
                  config.onEvent(feedItemToEvent(item));
                }
              } else if (sse.event === "tasks") {
                // Task events are arrays
                const tasks = parsed as LegacyTask[];
                for (const task of tasks) {
                  if (task.to_repo !== config.repo) continue;
                  if (task.status !== "pending") continue;
                  const key = `${task.id}:${task.status}`;
                  if (seenIds.has(key)) continue;
                  seenIds.add(key);
                  config.onEvent(taskToEvent(task));
                }
              }
            } catch {
              // Skip malformed JSON
            }
          }
        }
      }
    } catch {
      // Connection lost or refused
    }

    // Reconnect with backoff
    console.error(
      `[legion-channel] reconnecting in ${backoff / 1000}s`
    );
    await new Promise((resolve) => setTimeout(resolve, backoff));
    backoff = Math.min(backoff * 2, maxBackoff);
    connect();
  }

  connect();
}
