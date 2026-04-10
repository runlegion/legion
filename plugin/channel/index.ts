#!/usr/bin/env bun
import { Server } from "@modelcontextprotocol/sdk/server/index.js";
import { StdioServerTransport } from "@modelcontextprotocol/sdk/server/stdio.js";
import { connectSSE } from "./sse-client.js";
import { bridgeEvent } from "./event-bridge.js";
import { registerTools } from "./tools.js";
import { INSTRUCTIONS } from "./instructions.js";

const repo =
  process.env.LEGION_REPO ||
  process.env.CLAUDE_CWD?.split("/").pop() ||
  process.cwd().split("/").pop() ||
  "unknown";
const port = parseInt(process.env.LEGION_PORT || "3131", 10);
const fakechat = process.env.LEGION_FAKECHAT === "1";

const server = new Server(
  { name: "legion", version: "0.1.0" },
  {
    capabilities: {
      experimental: { "claude/channel": {} },
      tools: {},
    },
    instructions: INSTRUCTIONS,
  }
);

registerTools(server, { repo, port });

const transport = new StdioServerTransport();
await server.connect(transport);

// Write channel marker for hook coordination
const markerPath = `/tmp/legion-channel-${repo}`;
await Bun.write(markerPath, `${process.pid}`);

console.error(`[legion-channel] connected for repo: ${repo}, port: ${port}`);

// Push startup context (recall + status) through the channel on connect.
// This bypasses the broken SessionStart additionalContext pipeline.
async function pushStartupContext(): Promise<void> {
  try {
    const recallProc = Bun.spawn(["legion", "recall", "--repo", repo, "--latest", "--limit", "1"], {
      stdout: "pipe",
      stderr: "pipe",
    });
    const statusProc = Bun.spawn(["legion", "status", "--repo", repo, "--json"], {
      stdout: "pipe",
      stderr: "pipe",
    });

    const [recallResult, statusResult] = await Promise.all([
      new Response(recallProc.stdout).text(),
      new Response(statusProc.stdout).text(),
    ]);

    const parts: string[] = [];

    const recall = recallResult.trim();
    if (recall) parts.push(recall);

    const statusJson = statusResult.trim();
    if (statusJson) {
      try {
        const s = JSON.parse(statusJson);
        const counts: string[] = [];
        if (s.tasks > 0) {
          counts.push(s.blocked > 0 ? `${s.tasks} tasks (${s.blocked} blocked)` : `${s.tasks} tasks`);
        }
        if (s.team_needs > 0) counts.push(`${s.team_needs} unread @${repo}`);
        if (s.what_changed > 0) counts.push(`${s.what_changed} updates`);
        if (counts.length > 0) {
          parts.push(`[Legion] ${counts.join(", ")} -- legion bullpen for details`);
        }
      } catch {
        // skip malformed status
      }
    }

    if (parts.length > 0) {
      await server.notification({
        method: "notifications/claude/channel",
        params: {
          content: parts.join("\n\n"),
          meta: { type: "startup", from: "legion" },
        },
      });
      console.error("[legion-channel] startup context pushed");
    }
  } catch (err) {
    console.error("[legion-channel] startup context failed:", err);
  }
}

if (fakechat) {
  const { startFakechat } = await import("./fakechat.js");
  startFakechat((event) => bridgeEvent(server, event, repo));
} else {
  connectSSE({
    port,
    repo,
    onEvent: (event) => bridgeEvent(server, event, repo),
    onConnect: () => {
      console.error("[legion-channel] SSE stream connected");
      pushStartupContext();
    },
  });
}
