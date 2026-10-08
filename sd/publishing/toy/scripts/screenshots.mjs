// Serves dist/ with vite preview and screenshots each document page.
import { spawn } from "node:child_process";
import { chromium } from "playwright";

const PORT = 4179;
const DOCS = [
  ["intent", 1440],
  ["discovery", 1600],
  ["persona", 1440],
  ["journey", 1600],
  ["blueprint", 1920],
  ["ecosystem", 1600],
];

const server = spawn("pnpm", ["exec", "vite", "preview", "--port", String(PORT), "--strictPort"], {
  stdio: ["ignore", "pipe", "inherit"],
});
await new Promise((resolve) => {
  server.stdout.on("data", (chunk) => {
    if (String(chunk).includes(String(PORT))) resolve();
  });
});

const browser = await chromium.launch();
const errors = [];
try {
  for (const [key, width] of DOCS) {
    const page = await browser.newPage({ viewport: { width, height: 1000 } });
    page.on("pageerror", (err) => errors.push(`${key}: ${err.message}`));
    page.on("console", (msg) => {
      if (msg.type() === "error") errors.push(`${key}: ${msg.text()}`);
    });
    await page.goto(`http://localhost:${PORT}/#${key}`);
    await page.waitForSelector(`main[data-document="${key}"] h1`);
    await page.waitForTimeout(300);
    const missing = await page.locator("text=missing block type").count();
    if (missing > 0) errors.push(`${key}: ${missing} missing block types`);
    await page.screenshot({ path: `screenshots/${key}.png`, fullPage: true });
    const figure = page.locator("figure").first();
    if ((await figure.count()) > 0) await figure.screenshot({ path: `screenshots/${key}-figure.png` });
    await page.close();
    console.log(`screenshots/${key}.png`);
  }
} finally {
  await browser.close();
  server.kill();
}
if (errors.length > 0) {
  console.error(errors.join("\n"));
  process.exitCode = 1;
}
