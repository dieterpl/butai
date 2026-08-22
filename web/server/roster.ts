// The daemons this bridge speaks for.
//
// A daemon is a *socket path*. That is the daemon's whole contract — it listens
// on one AF_UNIX socket and never on TCP — and it is deliberately also the unit
// an ssh transport delivers into: `ssh -N -L <local>:<remote-socket> host` turns
// a far daemon into a local path, after which nothing here can tell it from a
// second daemon on this box.
//
// **This file is a deliberate transliteration of `server.py`, not a rewrite.**
// Key derivation, the socket-path allowlist and the collision rules are the
// bridge's only security boundary, and rewriting them idiomatically is where a
// traversal gets reintroduced. Where the Python is odd, it is odd here too, and
// the comment says why.

import { realpathSync, statSync } from "node:fs";
import { basename, dirname, isAbsolute } from "node:path";

export const DEFAULT_SOCKET = "/run/butai/butai.sock";

// A key is a short, stable name for one daemon and the namespace its ids live
// in. Kept to characters that are safe in a URL path segment and in a `key:id`
// split, so a qualified id never needs escaping.
const KEY_CHARS = /[^A-Za-z0-9._-]/g;

/** `<daemon-key>:<the daemon's own integer>` — see `resolveApiPath`. */
export const QID_RE = /^([A-Za-z0-9._-]+):([0-9]+)$/;

// Path components that say nothing about *which* machine a socket belongs to, so
// a key derived from a path walks past them: `/srv/gpu/.butai/butai.sock` is the
// gpu box, not the "butai" box.
const DULL_PARTS = new Set(["butai", ".butai", "bmux", ".bmux", "run", "var", "tmp", "home", "sock", "sockets"]);

/**
 * A name as Python's `{!r}` renders it — single quotes.
 *
 * Purely so the refusals read identically to the ones `server.py` has been
 * sending. The messages are the bridge's user interface when something is
 * misconfigured, and a client or a person grepping for one should not have to
 * care which implementation answered.
 */
export function repr(s: string): string {
  return s.includes("'") && !s.includes('"') ? `"${s}"` : `'${s.replace(/'/g, "\\'")}'`;
}

/** A request the bridge will not forward, with the reason to send back. */
export class Refused extends Error {
  constructor(
    readonly status: number,
    message: string,
  ) {
    super(message);
    this.name = "Refused";
  }
}

// Where a roster entry came from. The difference is not decoration: an entry
// from the environment comes back on the next restart whatever anyone does to
// it, so removing one would be a gesture that silently undoes itself — see
// `Roster.remove`. It is also the fact the MACHINES group needs in order to
// explain why one row offers a remove and another does not.
export type Source = "env" | "runtime";

export interface DaemonDto {
  key: string;
  label: string;
  socket: string;
  primary: boolean;
  source: Source;
  error: string | null;
  system: unknown;
}

/** One configured daemon: a key, a label and a socket path. */
export class DaemonRef {
  readonly label: string;

  constructor(
    readonly key: string,
    readonly socket: string,
    label?: string,
    readonly primary = false,
    readonly source: Source = "env",
  ) {
    this.label = label || key;
  }

  dto(error: string | null = null, system: unknown = null): DaemonDto {
    return {
      key: this.key,
      label: this.label,
      socket: this.socket,
      primary: this.primary,
      // `env` or `runtime`. Additive: a client that has never heard of it
      // reads exactly the document it read before.
      source: this.source,
      error,
      system,
    };
  }
}

/** A key with nothing in it that would break a path segment or a `key:id`. */
export function safeKey(name: string | null | undefined): string {
  const k = (name ?? "").trim().replace(KEY_CHARS, "-").replace(/^[-.]+|[-.]+$/g, "");
  return k.slice(0, 24) || "daemon";
}

/**
 * A key for a socket nobody named.
 *
 * The socket's own filename is the daemon's default (`butai.sock`) and says
 * nothing about the machine, so this walks up until it finds a component that
 * does. `/run/forwards/gpu-box.sock` -> `gpu-box`; `/srv/gpu/.butai/butai.sock`
 * -> `gpu`.
 */
export function deriveKey(path: string): string {
  const parts = path.replace(/\\/g, "/").split("/").filter((p) => p !== "" && p !== "." && p !== "..");
  for (let i = parts.length - 1; i >= 0; i--) {
    const p = parts[i]!;
    const stem = p.endsWith(".sock") ? p.slice(0, -5) : p;
    const bare = stem.toLowerCase().replace(/^\.+|\.+$/g, "");
    if (DULL_PARTS.has(bare) || !stem.replace(/^\.+|\.+$/g, "")) continue;
    return safeKey(stem);
  }
  return "daemon";
}

/**
 * The directories a **runtime-added** socket may live in.
 *
 * `POST /api/daemons` takes a path and connects to it, and this bridge has no
 * authentication of any kind — so without a boundary the route is "connect to
 * any AF_UNIX socket on this filesystem and speak a protocol at it" offered to
 * anyone who can reach the port. That is a smaller hole than the one the
 * daemon's own API already is, but it is a *new* one, and it is the kind that is
 * cheap to close now and awkward later.
 *
 * Entries are compared after `realpath`, so a symlink out of an allowed
 * directory does not count as being in it.
 */
export function socketDirs(env: Record<string, string | undefined>, daemons: readonly DaemonRef[]): string[] {
  const raw = (env.BUTAI_SOCKET_DIRS ?? "").trim();
  const dirs = raw
    ? raw.split(/[:,\s]+/).filter(Boolean)
    : [...daemons.map((d) => dirname(d.socket)), dirname(DEFAULT_SOCKET)];
  const out: string[] = [];
  for (const d of dirs) {
    let real: string;
    try {
      real = realpathSync(d);
    } catch {
      // Python's `os.path.realpath` resolves what exists and returns the rest
      // unchanged; Node's throws. Dropping the entry instead would *narrow* the
      // allowlist, which sounds safe and is a regression: `/run/butai` is the
      // container's conventional mount point and is routinely not there yet
      // when the bridge starts. Refusing to dial a socket that later appears
      // exactly where the deployment puts it is the bug this catches.
      real = d;
    }
    if (real && !out.includes(real)) out.push(real);
  }
  return out;
}

/**
 * Refuse a socket path that is not one this bridge is willing to dial.
 *
 * Three refusals, in the order that gives the most useful message: outside the
 * allowed directories, then not present, then present but not a socket. The last
 * two are separate because "you forwarded nothing here yet" and "that is a
 * regular file" are different mistakes with the same symptom.
 */
export function checkSocketPath(path: string, allowed: readonly string[]): string {
  if (!path || !isAbsolute(path)) {
    throw new Refused(400, `socket must be an absolute path, not ${repr(path)}`);
  }
  // Python's `os.path.realpath` resolves as far as it can and never throws, so
  // a socket that is not there yet still gets its *directory* resolved and
  // checked. Node's `realpathSync` throws on a missing leaf instead, and
  // falling back to the unresolved path would skip symlink resolution on the
  // parent — which is the one thing the allowlist depends on. So resolve the
  // parent (which exists) and re-attach the basename, which is what Python
  // computes.
  const real = realpathSync(dirname(path)) + "/" + basename(path);
  const parent = dirname(real);
  if (!allowed.includes(parent)) {
    throw new Refused(
      403,
      `this bridge will not dial ${path} — it is not in ${allowed.join(", ") || "any directory"}. ` +
        `Set BUTAI_SOCKET_DIRS to widen that, or put the forward in one of them`,
    );
  }
  // "You have not forwarded it yet" and "that is a regular file" are different
  // mistakes with one symptom, so they stay different answers.
  let st;
  try {
    st = statSync(real);
  } catch (e) {
    const err = e as NodeJS.ErrnoException;
    if (err.code === "ENOENT") {
      throw new Refused(404, `there is no socket at ${path} — forward it first (ssh -N -L)`);
    }
    throw new Refused(400, `cannot read ${path}: ${err.message}`);
  }
  if (!st.isSocket()) throw new Refused(400, `${path} is not a socket`);
  return real;
}

/**
 * Read the daemon list out of the environment.
 *
 * * `BUTAI_SOCKET` is the primary — on its own it is the whole configuration,
 *   so one daemon stays zero-config.
 * * `BUTAI_SOCKETS` names any others, comma- or whitespace-separated, each
 *   either `name=/path/to.sock` or a bare path whose key is derived.
 * * `BUTAI_SOCKET_NAME` renames the primary (it is `local` by default).
 *
 * Environment rather than a config file on purpose: the bridge is normally a
 * container whose sockets arrive as bind mounts, so the list is written where
 * the mounts are written. A file would be a second place for the same fact to be
 * wrong.
 */
export function parseDaemons(env: Record<string, string | undefined> = Bun.env): DaemonRef[] {
  const specs: Array<[string | null, string, boolean]> = [
    [(env.BUTAI_SOCKET_NAME ?? "").trim() || "local", env.BUTAI_SOCKET || DEFAULT_SOCKET, true],
  ];
  for (const spec of (env.BUTAI_SOCKETS ?? "").trim().split(/[,\s]+/)) {
    if (!spec) continue;
    const eq = spec.indexOf("=");
    const name = eq === -1 ? "" : spec.slice(0, eq);
    const path = eq === -1 ? "" : spec.slice(eq + 1);
    if (eq !== -1 && path.trim()) specs.push([safeKey(name), path.trim(), false]);
    else specs.push([null, spec, false]);
  }

  const out: DaemonRef[] = [];
  const seen = new Set<string>();
  for (const [name, path, primary] of specs) {
    let key = name ? safeKey(name) : deriveKey(path);
    // Two daemons under one key would make a qualified id ambiguous, which is
    // the one thing this whole scheme exists to prevent. Suffix instead.
    const base = key;
    let n = 2;
    while (seen.has(key)) key = `${base}-${n++}`;
    seen.add(key);
    out.push(new DaemonRef(key, path, undefined, primary));
  }
  return out;
}

/**
 * One request's picture of the roster, fixed for the whole of that request.
 *
 * **Every reader resolves against one of these rather than against the live
 * list**, and the reason is `resolveApiPath`: it walks a path segment by segment,
 * looking each key up as it goes, so `/api/workspaces/gpu:1/panes/gpu:5/ack`
 * consults the roster twice. A roster that changed in between would let one path
 * resolve its first id against one world and its second against another — the
 * wrong-machine bug the qualified ids exist to catch, arriving through the one
 * door they cannot watch, and rare enough to be untraceable when it did.
 *
 * The list is never empty: `parseDaemons` always yields the primary, and nothing
 * may remove the last daemon.
 */
export class RosterView {
  readonly daemons: readonly DaemonRef[];
  readonly primary: DaemonRef;
  private readonly byKey: Map<string, DaemonRef>;

  constructor(daemons: readonly DaemonRef[]) {
    this.daemons = [...daemons];
    this.byKey = new Map(this.daemons.map((d) => [d.key, d]));
    // The head of the list is `BUTAI_SOCKET`: the daemon an unqualified request
    // means, and the one whose `system` the snapshot reports at the top level.
    this.primary = this.daemons[0]!;
  }

  get length(): number {
    return this.daemons.length;
  }

  [Symbol.iterator](): Iterator<DaemonRef> {
    return this.daemons[Symbol.iterator]();
  }

  get(key: string): DaemonRef | undefined {
    return this.byKey.get(key);
  }

  /** Every key, sorted — for the "this bridge serves ..." refusals. */
  keys(): string[] {
    return [...this.byKey.keys()].sort();
  }
}

/**
 * The daemons this bridge speaks for.
 *
 * Read it through `view()`, which copies and hands back something that cannot
 * move. Python needed an `RLock` here; this does not, because Bun runs one
 * JavaScript thread and `add`/`remove` contain no `await` — a mutation is
 * therefore atomic against every reader by construction rather than by
 * discipline. The snapshot is still taken, for the reason `RosterView` gives:
 * one request must resolve every id it carries against one world.
 */
export class Roster {
  private daemons: DaemonRef[];

  constructor(daemons: DaemonRef[]) {
    this.daemons = [...daemons];
  }

  view(): RosterView {
    return new RosterView(this.daemons);
  }

  /**
   * Put a daemon on the roster, or refuse to.
   *
   * **A key collision is refused rather than suffixed.** `parseDaemons`
   * suffixes, and is right to: it is reading a list somebody wrote in one go, and
   * dropping an entry would lose a machine silently. This is one deliberate
   * request naming one machine, and answering it with a daemon under a key the
   * caller did not ask for is how a client ends up holding an id that means
   * nothing to it.
   *
   * **The same socket twice is refused too**: it would duplicate every one of
   * that daemon's projects in the tab bar, under two keys, with no way to tell
   * from a row which copy you are looking at. Paths are compared after
   * `realpath` (`checkSocketPath` returns one), so a symlink to a socket already
   * here is the same socket.
   */
  add(ref: DaemonRef): DaemonRef {
    for (const d of this.daemons) {
      if (d.key === ref.key) {
        throw new Refused(
          409,
          `there is already a daemon called ${repr(d.key)} on ${d.socket} — ` +
            `remove it first, or pass a different name`,
        );
      }
      let real = d.socket;
      try {
        real = realpathSync(d.socket);
      } catch {
        /* a socket that has gone away cannot collide with anything */
      }
      if (real === ref.socket) {
        throw new Refused(
          409,
          `${ref.socket} is already on this bridge as ${repr(d.key)} — ` +
            `adding it again would draw every one of its projects twice`,
        );
      }
    }
    this.daemons.push(ref);
    return ref;
  }

  /**
   * Drop a daemon, or say why it cannot be dropped.
   *
   * Two refusals. An **environment** entry comes back on the next restart
   * whatever happens here, so removing one is a gesture that silently undoes
   * itself — worse than a refusal, because it looks like it worked. And the
   * **last** daemon cannot go: `RosterView.primary` is the head of the list, and
   * every unqualified request, the top-level `system` and the startup banner are
   * written against there being one.
   */
  remove(key: string): DaemonRef {
    const i = this.daemons.findIndex((d) => d.key === key);
    if (i === -1) throw new Refused(404, `no daemon called ${repr(key)}`);
    const d = this.daemons[i]!;
    if (d.source === "env") {
      throw new Refused(
        409,
        `${key} was configured in the environment (${d.socket}), so removing it here would come ` +
          `back on the next restart — change BUTAI_SOCKET/BUTAI_SOCKETS instead`,
      );
    }
    if (this.daemons.length === 1) {
      throw new Refused(409, `${key} is the only daemon left; the bridge always has one`);
    }
    this.daemons.splice(i, 1);
    return d;
  }
}
