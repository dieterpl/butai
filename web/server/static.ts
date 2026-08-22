// The built client, served at `/`.
//
// This file used to be three clients wide. The vanilla custom-element app was a
// whitelist of twenty-odd basenames; `/ui/` was a second, half-built React
// client whose modules were guarded by a filename pattern; `/vendor/` was the
// CDN dependencies a build stage had walked into the image so the container
// needed no network. All three are gone, and the reason is the same reason each
// of them existed: there is a bundler now, and resolving imports is what a
// bundler does for a living.
//
// What is left is one directory of hashed assets and one `index.html`.

import { existsSync } from "node:fs";
import { dirname, extname, join } from "node:path";

// `web/`, from `web/server/`. Resolved from this module's own location rather
// than from the working directory, so `bun server/index.ts` works from anywhere
// — including inside the image, where the CWD is not the source tree.
const ROOT = dirname(dirname(Bun.fileURLToPath(import.meta.url)));
const DIST = join(ROOT, "dist");

const TYPES: Record<string, string> = {
  ".html": "text/html; charset=utf-8",
  ".js": "text/javascript; charset=utf-8",
  ".css": "text/css; charset=utf-8",
  ".svg": "image/svg+xml",
  ".json": "application/json",
  ".map": "application/json",
  ".woff2": "font/woff2",
  ".png": "image/png",
  ".ico": "image/x-icon",
};

// One path segment, a leading character that cannot start `..`, and an
// extension from the table above. Traversal is closed by the pattern rather
// than by normalising afterwards, which is the same rule the old whitelist used
// and the same reason: `join(root, name)` can then only ever name a file
// directly in that directory.
const SAFE = /^[A-Za-z0-9][A-Za-z0-9._-]*$/;

// The browser probes /favicon.ico on every load whether or not the page asks
// for one, and a 404 there was the only console error on an otherwise working
// page — which made the console useless as a signal. Served inline: no file to
// keep in sync, nothing to forget. A prompt caret, the smallest honest picture
// of a terminal.
const FAVICON =
  '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16">' +
  '<rect width="16" height="16" rx="3" fill="#0b0e13"/>' +
  '<path d="M3.5 4.5L6.5 8l-3 3.5" fill="none" stroke="#58a6ff" stroke-width="1.7"' +
  ' stroke-linecap="round" stroke-linejoin="round"/>' +
  '<path d="M8.5 11.5h4" fill="none" stroke="#58a6ff" stroke-width="1.7"' +
  ' stroke-linecap="round"/>' +
  "</svg>";

async function file(path: string): Promise<Response | null> {
  const f = Bun.file(path);
  if (!(await f.exists())) return null;
  const type = TYPES[extname(path)] ?? "application/octet-stream";
  // The bundle's filenames carry a content hash, so they can be cached for a
  // year; `index.html` names them and must never be. Getting this the wrong way
  // round is how a deploy serves last week's app out of a browser cache.
  const cache = path.endsWith("index.html")
    ? "no-cache"
    : "public, max-age=31536000, immutable";
  return new Response(f, { headers: { "Content-Type": type, "Cache-Control": cache } });
}

/**
 * Serve `path`, or null if this is not a static route at all.
 *
 * Returning null rather than a 404 matters: the caller has API routes to try
 * after this one, and a 404 here would shadow them.
 *
 * **Anything that is not a file is `index.html`.** The client is a single page
 * that keeps its own route, so a reload anywhere has to reach the same
 * document — the 404-on-reload every SPA ships once.
 */
export async function serveStatic(path: string): Promise<Response | null> {
  if (path === "/favicon.ico" || path === "/favicon.svg") {
    return new Response(FAVICON, {
      headers: { "Content-Type": "image/svg+xml", "Cache-Control": "public, max-age=86400" },
    });
  }
  if (!existsSync(join(DIST, "index.html"))) return null;

  const rest = path.replace(/^\/+/, "");
  if (rest) {
    // `assets/` is the only subdirectory Vite emits, so it is the only one
    // reachable — a second segment anywhere else is not a path this serves.
    const [head, tail, ...more] = rest.split("/");
    if (!more.length) {
      const dir = tail === undefined ? DIST : head === "assets" ? join(DIST, "assets") : null;
      const name = tail === undefined ? head : tail;
      if (dir && name && SAFE.test(name)) {
        const hit = await file(join(dir, name));
        if (hit) return hit;
      }
    }
  }
  return file(join(DIST, "index.html"));
}

/** Whether there is a built client to serve at all — the banner says so. */
export function builtClientPresent(): boolean {
  return existsSync(join(DIST, "index.html"));
}
