// The client's build. Tailwind v4 is a Vite plugin now rather than a PostCSS
// step, which is why there is no postcss.config.js and no tailwind.config.js —
// the theme lives in `src/styles.css` under `@theme`, next to the CSS it
// configures.
import { fileURLToPath, URL } from "node:url";
import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";

export default defineConfig({
  // Relative, so the bundle works wherever it is mounted. The bridge serves it
  // at `/app/` today and at `/` after the cut-over, and an absolute `/assets/`
  // would 404 under the first of those. Safe because the client keeps its page
  // in React state rather than in the URL, so there is no nested path for a
  // relative reference to resolve against wrongly.
  base: "./",
  plugins: [react(), tailwindcss()],
  resolve: {
    alias: { "@": fileURLToPath(new URL("./src", import.meta.url)) },
  },
  build: { outDir: "dist", emptyOutDir: true },
  server: {
    port: 5173,
    // `bun run bridge` on 8080 owns the daemon; the dev server owns only the
    // assets. Same-origin in production (the bridge serves `dist/`), so this
    // proxy exists purely so `vite dev` behaves like the shipped thing.
    proxy: {
      "/api": { target: "http://127.0.0.1:8080", changeOrigin: true },
      "/ws": { target: "ws://127.0.0.1:8080", ws: true },
    },
  },
});
