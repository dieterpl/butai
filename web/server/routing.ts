// Which daemon a request means, and what path it becomes.
//
// Transliterated from `server.py`'s `qid` / `resolve_api_path` /
// `daemon_from_query`. Every refusal here is a bug that would otherwise be
// silent, which is why none of them is relaxed in the port.

import { QID_RE, Refused, repr, type DaemonRef, type RosterView } from "./roster.ts";

/**
 * `<daemon-key>:<n>` — the only way an id crosses to the browser.
 *
 * Workspace ids and pane ids are per-daemon integers, so two daemons both have a
 * workspace 1 and both have a pane 5. A bare integer cannot say which, and the
 * failure it produces is silent: you attach to the wrong machine's pane and it
 * looks like a working terminal. A string can say which, and it has a second
 * property that an `{id, daemon}` pair does not — a bare int compared against it
 * never matches, so code that forgot to qualify renders nothing instead of
 * rendering someone else's machine.
 */
export function qid(key: string, n: number | null | undefined): string | null {
  return n === null || n === undefined ? null : `${key}:${n}`;
}

export function daemonByKey(view: RosterView, key: string, where: string): DaemonRef {
  const d = view.get(key);
  if (!d) {
    throw new Refused(
      400,
      `no daemon called ${repr(key)} (${where}); this bridge serves ` +
        `${view.keys().join(", ") || "none"}`,
    );
  }
  return d;
}

/**
 * Turn a browser `/api/...` path into a daemon and its `/v1/...` path.
 *
 * **The whole namespacing rule lives here.** Any path segment shaped `<key>:<n>`
 * names a daemon and one of its ids; it picks the daemon and is rewritten to the
 * bare `<n>` the daemon understands. So `/api/workspaces/gpu:1/panes/gpu:5/ack`
 * reaches `gpu` as `/v1/workspaces/1/panes/5/ack`, and the client never has to
 * know that the daemon speaks bare integers.
 *
 * Three refusals, and each of them is a bug that would otherwise be silent:
 *
 *   * a segment naming a daemon this bridge does not have
 *   * two segments naming *different* daemons in one path — an id from one
 *     machine used against another, which is the mistake this scheme exists to
 *     catch
 *   * a bare integer id while more than one daemon is configured, unless
 *     `?daemon=` says which. With one daemon a bare id is unambiguous and is
 *     still accepted, so `curl /api/workspaces/1/tree` keeps working.
 *
 * `?daemon=<key>` selects the daemon for the routes that have no workspace in
 * them at all (`/api/agents`, `/api/notifications`, `/api/fs`, `POST
 * /api/workspaces`). It is consumed here and never forwarded.
 *
 * `view` is this request's `RosterView` and is consulted more than once below,
 * which is the whole reason it is a snapshot rather than the live list.
 */
export function resolveApiPath(view: RosterView, path: string): { daemon: DaemonRef; path: string } {
  const q = path.indexOf("?");
  const raw = q === -1 ? path : path.slice(0, q);
  const query = q === -1 ? "" : path.slice(q + 1);
  const rest = raw.slice("/api".length) || "/";

  let asked: string | null = null;
  const keep: string[] = [];
  for (const p of query.split("&")) {
    if (!p) continue;
    const eq = p.indexOf("=");
    const k = eq === -1 ? p : p.slice(0, eq);
    const v = eq === -1 ? "" : p.slice(eq + 1);
    if (k === "daemon") asked = unquote(v);
    else keep.push(p);
  }

  const segs = rest.split("/").filter(Boolean);
  let picked: DaemonRef | null = null;
  const out: string[] = [];
  for (const s of segs) {
    const m = QID_RE.exec(s);
    if (!m) {
      out.push(s);
      continue;
    }
    const [, key, n] = m as unknown as [string, string, string];
    const d = daemonByKey(view, key, `in /${segs.join("/")}`);
    if (picked && picked.key !== d.key) {
      throw new Refused(
        400,
        `the path mixes daemons (${picked.key} and ${d.key}): /${segs.join("/")} — ` +
          `an id from one machine cannot be used against another`,
      );
    }
    picked = d;
    out.push(n);
  }

  if (asked !== null) {
    const d = daemonByKey(view, asked, "in ?daemon=");
    if (picked && picked.key !== d.key) {
      throw new Refused(400, `?daemon=${d.key} disagrees with the ids in the path, which name ${picked.key}`);
    }
    picked = picked ?? d;
  }

  if (!picked) {
    // A bare id can only mean one thing when there is one daemon. With several
    // it is the wrong-machine bug in its exact form, so it stops here rather
    // than reaching whichever daemon happens to be first.
    const bare = out.filter((s) => /^[0-9]+$/.test(s));
    if (bare.length && view.length > 1) {
      throw new Refused(
        400,
        `unqualified id ${bare[0]} in /${segs.join("/")} — this bridge serves ${view.length} daemons, ` +
          `so an id must be written <daemon>:<id> (or the request must carry ?daemon=<key>)`,
      );
    }
    picked = view.primary;
  }

  let tail = out.length ? "/v1/" + out.join("/") : "/v1";
  if (keep.length) tail += "?" + keep.join("&");
  return { daemon: picked, path: tail };
}

/** Minimal percent-decoding for a query value (keys are URL-safe anyway). */
export function unquote(s: string): string {
  try {
    return decodeURIComponent(s);
  } catch {
    return s;
  }
}

/**
 * The daemon named by `?daemon=` on a bridge-local route (`/ws`, `/api/events`).
 *
 * With one daemon configured, no parameter means that daemon — which is what
 * keeps the single-daemon client working with no change at all. With several, an
 * unnamed daemon is refused rather than defaulted: defaulting is how a client
 * that forgot to say which machine silently attaches to the wrong one, and this
 * route's whole job is to reach a *particular* one.
 */
export function daemonFromQuery(view: RosterView, path: string, where: string): DaemonRef {
  const q = path.indexOf("?");
  const query = q === -1 ? "" : path.slice(q + 1);
  for (const p of query.split("&")) {
    const eq = p.indexOf("=");
    if (eq === -1) continue;
    if (p.slice(0, eq) === "daemon" && p.slice(eq + 1)) {
      return daemonByKey(view, unquote(p.slice(eq + 1)), where);
    }
  }
  if (view.length > 1) {
    throw new Refused(400, `${where} needs ?daemon=<key> — this bridge serves ${view.keys().join(", ")}`);
  }
  return view.primary;
}
