import { defineConfig } from "vite";
import { resolve } from "node:path";

// Dev-only config. In production the legion binary serves the build output
// embedded via rust-embed -- Vite never runs at runtime. The dev server
// proxies the API + SSE surface to a running `legion daemon`/`legion serve`
// on :3131 so the frontend talks to real data while we iterate.
export default defineConfig({
  resolve: {
    alias: {
      "@": resolve(__dirname, "src"),
    },
  },
  server: {
    port: 4000,
    proxy: {
      "/api": "http://localhost:3131",
      "/sse": "http://localhost:3131",
    },
  },
  build: {
    outDir: "dist",
    emptyOutDir: true,
  },
});
