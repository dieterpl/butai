// The butai web bridge — from butai Unix sockets to a browser.
//
// A browser cannot open an AF_UNIX socket, so this server (typically a container
// with the butai socket bind-mounted in, the same pattern as mounting
// /var/run/docker.sock) does four jobs:
//
//   * serves the client at `/`
//   * relays the daemon's **framed streaming protocol** over a WebSocket at
//     `/ws` — the client draws its chrome from the REST API and streams only ONE
//     pane, the centre "stage", over this socket
//   * proxies `/api/*` to the daemon's HTTP-over-socket API (`/v1/*`), and
//     aggregates a whole-world snapshot at `/api/state`
//   * relays the daemon's push channel, `GET /v1/events`, to the browser as
//     Server-Sent Events at `/api/events`
//
// The daemon itself is never modified: it speaks only over its AF_UNIX socket
// (framed + HTTP on the same socket, routed by a first-byte sniff). This bridge
// is a pure translator, which keeps the daemon's one-socket contract intact —
// forward the socket over SSH and the browser client works against a remote host
// unchanged.
//
// **Several daemons, one browser.** The TUI's tab bar spans machines, and so
// does this: the bridge holds a *list* of sockets. Every read that is "the whole
// world" is the union across them, a daemon that is down is a marker rather than
// an exception, and every id that crosses to the browser is qualified with the
// daemon it came from — see `routing.ts` for the whole of that rule. The
// single-daemon case needs no configuration.
//
// **No dependencies.** Bun's `fetch` speaks to a Unix socket and `Bun.serve`
// speaks WebSocket, which between them are the two things that made the Python
// version 1,503 lines. A bridge is not a place that needs a framework.

import {
  Refused,
  Roster,
  DaemonRef,
  checkSocketPath,
  deriveKey,
  parseDaemons,
  safeKey,
  socketDirs,
} from "./roster.ts";
import { daemonFromQuery, resolveApiPath, unquote } from "./routing.ts";
import { butaiRequest } from "./proxy.ts";
import { snapshot } from "./snapshot.ts";
import { relayEvents } from "./events.ts";
import { websocket, type WsData } from "./ws.ts";
import { serveStatic, builtClientPresent } from "./static.ts";
import { json, refused, NOT_FOUND } from "./reply.ts";

const ROSTER = new Roster(parseDaemons());

// How long the dial probe waits. Short: a socket that is really a daemon answers
// immediately, and somebody is holding a form open waiting to hear.
const DIAL_TIMEOUT_MS = 5_000;

/**
 * Confirm there is a butai daemon behind a socket, or say what is there instead.
 *
 * **Dialled before it is added, and that ordering is the feature.** An entry on
 * the roster that has never answered is indistinguishable from a machine that
 * was fine and has just gone down — both draw as "unreachable" — and the
 * difference matters most at exactly the moment somebody is typing a path and
 * wants to know whether they got it right. So a machine that cannot be reached
 * never joins the roster at all, and the reason goes back in the reply.
 */
async function dialProbe(ref: DaemonRef): Promise<DaemonRef> {
  let r;
  try {
    r = await butaiRequest(ref, "GET", "/v1/workspaces", undefined, "application/json", DIAL_TIMEOUT_MS);
  } catch (e) {
    throw new Refused(502, `nothing answered on ${ref.socket}: ${e instanceof Error ? e.message : String(e)}`);
  }
  if (r.status !== 200) {
    throw new Refused(502, `${ref.socket} answered ${r.status} to GET /v1/workspaces — that is not a butai daemon`);
  }
  let parsed: unknown;
  try {
    parsed = JSON.parse(new TextDecoder().decode(r.body) || "[]");
  } catch {
    throw new Refused(502, `${ref.socket} answered something that is not JSON — that is not a butai daemon`);
  }
  if (!Array.isArray(parsed)) {
    throw new Refused(502, `${ref.socket} did not answer GET /v1/workspaces with a list`);
  }
  return ref;
}

/**
 * `POST /api/daemons` — put one more machine in this bridge's tab bar.
 *
 * The body is `[[remote]]` from the TUI's config, field for field (`name`,
 * `socket`), because that is already the vocabulary for "another daemon" and a
 * second spelling would be a second thing to learn for one idea.
 *
 * `host` is accepted as a *name* here only to refuse it with the reason: it is
 * the field somebody will reach for first, and "unknown field" would send them
 * looking for a typo instead of at the sentence explaining that this bridge does
 * not dial ssh.
 */
async function addDaemon(body: unknown): Promise<DaemonRef> {
  if (!body || typeof body !== "object" || Array.isArray(body)) {
    throw new Refused(400, "expected a JSON object");
  }
  const b = body as Record<string, unknown>;
  if (b.host) {
    throw new Refused(
      400,
      "this bridge does not run ssh — forward the far socket with " +
        `\`ssh -N -L <local>:<remote-socket> ${String(b.host)}\` and pass that local path as \`socket\``,
    );
  }
  const path = typeof b.socket === "string" ? b.socket.trim() : "";
  if (!path) {
    throw new Refused(400, "a daemon needs a `socket`: the path to an already-reachable butai socket");
  }

  const view = ROSTER.view();
  const real = checkSocketPath(path, socketDirs(Bun.env, view.daemons));
  const name = typeof b.name === "string" && b.name.trim() ? b.name : null;
  // The key is URL-safe and derived when unnamed; the *label* keeps whatever the
  // caller actually typed, so the MACHINES row reads as the person named it.
  const ref = new DaemonRef(name ? safeKey(name) : deriveKey(real), real, name ?? undefined, false, "runtime");
  await dialProbe(ref);
  return ROSTER.add(ref);
}

async function handle(req: Request, server: Bun.Server<WsData>): Promise<Response> {
  const url = new URL(req.url);
  const path = url.pathname;
  // The path *and* its query, which is what the qualified-id resolver reads.
  const full = path + url.search;

  // One view for this request, taken before anything is resolved against it.
  // `/ws` and `/api/events` in particular outlive the roster they were dialled
  // from — they hold a socket, not a key — so the machine a connection is
  // attached to is decided once, here.
  const view = ROSTER.view();

  if (path === "/ws") {
    let ref;
    try {
      ref = daemonFromQuery(view, full, "/ws");
    } catch (e) {
      return refused(e);
    }
    if (server.upgrade(req, { data: { daemon: ref } })) {
      return new Response(null, { status: 101 });
    }
    return json(400, { error: "expected websocket upgrade" });
  }

  if (req.method === "GET") {
    // The roster, and nothing else — no daemon is contacted. The client needs to
    // know how many streams to open *before* it opens any, and that question
    // must not cost a round trip to a machine that may be asleep.
    if (path === "/api/daemons") return json(200, { daemons: view.daemons.map((d) => d.dto()) });

    if (path === "/api/events") {
      try {
        return await relayEvents(daemonFromQuery(view, full, "/api/events"), req.headers.get("Last-Event-ID"));
      } catch (e) {
        return refused(e);
      }
    }

    if (path === "/api/state") return json(200, await snapshot(view));

    // Static before the proxy, and routed on the path alone: a query string is
    // the caller's business (a cache-buster on /index.html?v=2, say) and must
    // not turn a known route into a 404 — or, worse, drop /api/state through to
    // the proxy, where it becomes /v1/state and the daemon 404s it.
    if (!path.startsWith("/api/")) {
      const hit = await serveStatic(path);
      return hit ?? NOT_FOUND();
    }
  }

  if (req.method === "POST" && path === "/api/daemons") {
    // Ahead of the proxy: `/api/daemons` is the bridge's own, and the daemon has
    // no `/v1/daemons` to forward it to.
    let parsed: unknown;
    try {
      parsed = JSON.parse((await req.text()) || "{}");
    } catch (e) {
      return json(400, { error: `body is not JSON: ${e instanceof Error ? e.message : String(e)}` });
    }
    try {
      return json(200, (await addDaemon(parsed)).dto());
    } catch (e) {
      return refused(e);
    }
  }

  if (req.method === "DELETE" && path.startsWith("/api/daemons/")) {
    try {
      return json(200, { removed: ROSTER.remove(unquote(path.slice("/api/daemons/".length))).dto() });
    } catch (e) {
      return refused(e);
    }
  }

  if (path.startsWith("/api/")) return proxyToDaemon(req, view, full);

  return NOT_FOUND();
}

/** Everything else under `/api/`, forwarded to the daemon it names. */
async function proxyToDaemon(req: Request, view: ReturnType<Roster["view"]>, full: string): Promise<Response> {
  let target;
  try {
    target = resolveApiPath(view, full);
  } catch (e) {
    return refused(e);
  }
  const body = req.method === "GET" || req.method === "DELETE" ? undefined : new Uint8Array(await req.arrayBuffer());
  let r;
  try {
    r = await butaiRequest(
      target.daemon,
      req.method,
      target.path,
      body,
      req.headers.get("Content-Type") ?? "application/json",
    );
  } catch (e) {
    return json(502, { error: `butai socket: ${e instanceof Error ? e.message : String(e)}` });
  }
  const headers: Record<string, string> = {
    "Content-Type": r.headers.get("content-type") ?? "application/json",
  };
  // Carried through so a file download saves with the right name and MIME.
  const cd = r.headers.get("content-disposition");
  if (cd) headers["Content-Disposition"] = cd;
  return new Response(r.body as BodyInit, { status: r.status, headers });
}

const port = Number(Bun.env.PORT ?? "8080");
const server = Bun.serve<WsData>({
  port,
  idleTimeout: 0, // an event stream and a pane socket are both meant to sit quiet
  fetch: handle,
  websocket,
  error(e) {
    return json(500, { error: e.message });
  },
});

const where = ROSTER.view()
  .daemons.map((d) => `${d.key}=${d.socket}`)
  .join(", ");
console.log(
  `butai web bridge on :${server.port}, bridging ${where}` + (builtClientPresent() ? "" : " (no built client — run `bun run build`)"),
);
