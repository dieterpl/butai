# butai web client — the bridge and the built client, in one image.
#
# The client has a build step now, and that is what makes this file simple
# rather than complicated. The version before it had **three stages** whose only
# job was to make the image work without a network: React, htm and Radix were
# imported from a CDN at page load, so a build stage walked the esm.sh module
# graph into `vendor/`, a second rewrote every import map to point at those
# files, and a script checked that no bare specifier had been missed. A
# container that reaches a registry the first time somebody opens the page fails
# in exactly the environment it exists for — Caliper ships this image.
#
# `bun install --frozen-lockfile` and `bun run build` do all of that, honestly,
# because a bundler resolves imports for a living. `vendor.txt`, `tools/vendor.py`
# and the offline probe are gone with it.


# ── 1. dependencies ──────────────────────────────────────────────────────────
# Only the manifest and the lockfile, so editing the client does not re-resolve
# the tree: this layer is cached against the two files that can change what it
# installs. `--frozen-lockfile` makes the lockfile a fact rather than a hint —
# an image whose dependencies drift from the repo's is not reproducible.
FROM oven/bun:1 AS deps
WORKDIR /app
COPY package.json bun.lock ./
RUN bun install --frozen-lockfile


# ── 2. the client, built ─────────────────────────────────────────────────────
# Vite emits a hashed bundle into `dist/`. Nothing it references leaves the
# image, which is the whole of the offline guarantee that used to need a stage
# of its own.
FROM deps AS build
WORKDIR /app
COPY . .
RUN bun run build


# ── 3. what ships ────────────────────────────────────────────────────────────
# The bridge runs from source — Bun executes TypeScript directly, so there is no
# server build and no second bundle to keep in step with the first. It needs its
# own dependencies at runtime: none. `server/` imports nothing outside the
# standard library and its own files, which is why `node_modules` is not copied.
FROM oven/bun:1-slim
WORKDIR /app
COPY --from=build /app/dist ./dist
COPY --from=build /app/server ./server
COPY --from=build /app/src/logic ./src/logic
COPY --from=build /app/src/protocol ./src/protocol

ENV PORT=8080
ENV BUTAI_SOCKET=/run/butai/butai.sock
EXPOSE 8080

# No shell form: this is PID 1 and it should receive signals directly, so
# `docker stop` closes the daemon sockets instead of waiting out the timeout.
CMD ["bun", "server/index.ts"]
