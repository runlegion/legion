import { fileURLToPath, URL } from "node:url";
import tailwindcss from "@tailwindcss/vite";
import react from "@vitejs/plugin-react";
import { defineConfig } from "vite";

// https://vite.dev/config/
export default defineConfig({
  plugins: [react(), tailwindcss()],
  resolve: {
    alias: {
      "@": fileURLToPath(new URL("./src", import.meta.url)),
    },
  },
  server: {
    proxy: {
      // The plugin's daemon on 3131. Point LEGION_API at another port to prove
      // against a daemon built from an unmerged branch.
      "/api": process.env["LEGION_API"] ?? "http://localhost:3131",
    },
  },
});
