// The old pane renderer and the new one, fed identical frames, compared pixel
// for pixel.
//
// This is `compare-bridges.sh`'s argument applied to a canvas. The port is only
// correct if the two draw the same thing, and asserting on the *drawing* would
// mean writing down what I believe `butai-screen.js` puts on screen — which is
// the thing under test. So both run in one browser against one fixture list and
// the image data is diffed.
//
// Both need a real browser: `getImageData`, `requestAnimationFrame`,
// `devicePixelRatio` and font metrics have no meaningful stand-in. Playwright's
// chromium, because the local Chrome is 73.
//
// Usage: bun test/compare-renderers.mjs [--write-diffs <dir>]

import { chromium } from "playwright";
import { createServer } from "node:http";
import { readFile } from "node:fs/promises";
import { dirname, extname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const HERE = dirname(fileURLToPath(import.meta.url));
const APP = dirname(HERE);
const WEB = dirname(APP);

const TYPES = { ".js": "text/javascript", ".mjs": "text/javascript", ".html": "text/html", ".css": "text/css" };

// One server rooted at `web/`, so `/butai-screen.js` is the original and
// `/app/...` reaches the port. Same origin for both, which module scripts need.
const server = createServer(async (req, res) => {
  const url = new URL(req.url, "http://x");
  const path = resolve(join(WEB, decodeURIComponent(url.pathname)));
  if (!path.startsWith(WEB)) {
    res.writeHead(403).end("no");
    return;
  }
  try {
    const body = await readFile(path);
    res.writeHead(200, { "Content-Type": TYPES[extname(path)] ?? "application/octet-stream" }).end(body);
  } catch {
    res.writeHead(404).end("not found");
  }
});
await new Promise((r) => server.listen(0, "127.0.0.1", r));
const base = `http://127.0.0.1:${server.address().port}`;

const browser = await chromium.launch();
const page = await browser.newPage({ viewport: { width: 900, height: 500 }, deviceScaleFactor: 2 });
page.on("pageerror", (e) => console.log("  page error:", e.message));

await page.goto(`${base}/app/test/renderer-harness.html`, { waitUntil: "networkidle" });

const results = await page.evaluate(() => window.__compare());

let failed = 0;
for (const r of results) {
  if (r.error) {
    console.log(`  ERROR ${r.name}: ${r.error}`);
    failed++;
  } else if (r.diff === 0) {
    console.log(`  ok    ${r.name}  (${r.pixels} px)`);
  } else {
    const pct = ((r.diff / r.pixels) * 100).toFixed(3);
    console.log(`  FAIL  ${r.name}: ${r.diff}/${r.pixels} pixels differ (${pct}%)`);
    if (r.firstAt) console.log(`          first at (${r.firstAt.x},${r.firstAt.y}) old=${r.firstAt.old} new=${r.firstAt.new}`);
    failed++;
  }
}

await browser.close();
server.close();

console.log(failed ? `\n${failed} of ${results.length} differ` : `\nIDENTICAL across ${results.length} fixtures`);
process.exit(failed ? 1 : 0);
