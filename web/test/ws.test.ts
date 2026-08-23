// The `/ws` relay, against a real daemon.
//
// This is the one part of the bridge the comparison harness cannot cover by
// diffing replies: a WebSocket is not a request. It is also the part with
// hand-written framing on both sides — Bun supplies RFC 6455, but the daemon's
// own 4-byte length prefix is ours — so it is exactly where a byte-order slip or
// a reassembly bug would live and would present as a stage that never paints.
//
// The two directions are proved separately by one exchange. A `hello` that
// reaches the daemon at all proves the browser->daemon encode (a wrong length
// and the daemon reads garbage and hangs up); a `hello` coming *back* proves the
// daemon->browser decode.

import { afterAll, beforeAll, expect, test } from "bun:test";
import { existsSync, mkdtempSync, rmSync, statSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

// The daemon binary under test, named explicitly by `BUTAI_BIN`.
//
// This used to fall back to `/var/tmp/butai-probe/butai`, which is why the
// suite was green on a developer box and red on every clean one. Worse than
// the CI failure: that path holds whatever binary was last dropped there, so
// locally these tests were proving the relay against a months-old daemon while
// reporting on the current tree. A path nobody set is not a default worth
// having — CI's integration job builds one and points this at it.
const BUTAI = Bun.env.BUTAI_BIN ?? "";
const HAVE_BUTAI = BUTAI !== "" && existsSync(BUTAI);
if (!HAVE_BUTAI) {
  console.warn(
    "ws.test.ts: skipping — set BUTAI_BIN to a butai binary to run the relay tests",
  );
}
const PORT = 8093;

let run: string;
let sock: string;
let daemon: Bun.Subprocess | undefined;
let bridge: Bun.Subprocess | undefined;

// A short root: the socket path has to fit inside `sockaddr_un.sun_path`, and
// the default temp dir plus a random suffix is already most of the budget.
function shortTmp(): string {
  return mkdtempSync(join(tmpdir().length > 12 ? "/tmp" : tmpdir(), "btw"));
}

async function waitFor(what: string, probe: () => Promise<boolean> | boolean, ms = 15_000) {
  const until = Date.now() + ms;
  while (Date.now() < until) {
    if (await probe()) return;
    await Bun.sleep(150);
  }
  throw new Error(`timed out waiting for ${what}`);
}

beforeAll(async () => {
  if (!HAVE_BUTAI) return;
  run = shortTmp();
  sock = join(run, "d.sock");
  // `BUTAI` (no suffix) is exported by a butai pane and holds a *socket* path,
  // so the whole family is cleared rather than trusted — an "isolated" daemon
  // that inherits it aims at the user's real one and spawns their agents.
  // `Record<string, string>` rather than `| undefined`: adding `vite/client`
  // types put `ImportMetaEnv`'s optional members into `Bun.env`, and under
  // `exactOptionalPropertyTypes` an optional `string` is not a `string |
  // undefined` index. Deleting a key is what this needs, not writing one.
  const env = { ...Bun.env, HOME: run, BUTAI_SOCKET: sock } as unknown as Record<string, string>;
  for (const k of ["BUTAI", "BUTAI_PANE", "BUTAI_WORKSPACE", "BUTAI_SOCKETS", "BUTAI_SOCKET_DIRS"]) {
    delete env[k];
  }

  daemon = Bun.spawn([BUTAI, "daemon"], { env, stdout: "ignore", stderr: "ignore" });
  // `existsSync`, not `Bun.file(sock).exists()`: the latter answers *false* for
  // a Unix socket, because it reports whether the path is a regular file. The
  // daemon was up the whole time and the probe was the thing that was wrong,
  // which reads exactly like a daemon that will not start.
  await waitFor("the daemon socket", () => existsSync(sock) && statSync(sock).isSocket());

  bridge = Bun.spawn(["bun", join(import.meta.dir, "..", "server", "index.ts")], {
    env: { ...env, PORT: String(PORT) },
    stdout: "ignore",
    stderr: "ignore",
  });
  await waitFor("the bridge", async () => {
    try {
      return (await fetch(`http://127.0.0.1:${PORT}/api/daemons`)).ok;
    } catch {
      return false;
    }
  });
});

afterAll(async () => {
  if (!HAVE_BUTAI) return;
  bridge?.kill();
  daemon?.kill();
  // By socket, never by pattern: `pkill -f butai` matches the user's own daemon.
  if (sock && existsSync(sock)) {
    Bun.spawnSync([BUTAI, "--socket", sock, "kill-server"], { stdout: "ignore", stderr: "ignore" });
  }
  if (run) rmSync(run, { recursive: true, force: true });
});

/** One round trip through the relay: send a ClientMsg, read the first ServerMsg. */
function exchange(msg: unknown, timeoutMs = 10_000): Promise<Record<string, unknown>> {
  return new Promise((resolve, reject) => {
    const ws = new WebSocket(`ws://127.0.0.1:${PORT}/ws`);
    const timer = setTimeout(() => {
      ws.close();
      reject(new Error("no frame came back within " + timeoutMs + "ms"));
    }, timeoutMs);
    ws.onopen = () => ws.send(JSON.stringify(msg));
    ws.onmessage = (e) => {
      clearTimeout(timer);
      ws.close();
      try {
        // A *text* frame carrying JSON. If the relay ever sent the payload as
        // bytes this would be a Blob and the parse would throw, which is the
        // regression the assertion is really guarding.
        expect(typeof e.data).toBe("string");
        resolve(JSON.parse(e.data as string) as Record<string, unknown>);
      } catch (err) {
        reject(err);
      }
    };
    ws.onerror = () => {
      clearTimeout(timer);
      reject(new Error("websocket errored"));
    };
  });
}

test.skipIf(!HAVE_BUTAI)("a hello crosses the relay and the daemon's hello comes back", async () => {
  const reply = await exchange({
    hello: { proto_version: 1, encoding: "json", cols: 80, rows: 24, target: "default", cwd: "/" },
  });
  // Externally tagged, so the variant *is* the key. Anything at all coming back
  // proves both framing directions; `hello` proves the daemon understood it
  // rather than merely failing politely.
  expect(Object.keys(reply)[0]).toBe("hello");
  const hello = reply.hello as Record<string, unknown>;
  expect(hello.proto_version).toBe(1);
});

test.skipIf(!HAVE_BUTAI)("a message sent before the daemon socket is dialled is not dropped", async () => {
  // The upgrade completes immediately; `Bun.connect` does not. The client's
  // `hello` lands in exactly that window, so the bridge queues rather than
  // drops. Sending on `open` — which is what `exchange` does — *is* the race,
  // so this passing at all is the queue working.
  const replies = await Promise.all([
    exchange({ hello: { proto_version: 1, encoding: "json", cols: 80, rows: 24, target: "default", cwd: "/" } }),
    exchange({ hello: { proto_version: 1, encoding: "json", cols: 100, rows: 30, target: "default", cwd: "/" } }),
  ]);
  for (const r of replies) expect(Object.keys(r)[0]).toBe("hello");
});

test.skipIf(!HAVE_BUTAI)("an unknown daemon key is refused before any socket is dialled", async () => {
  const res = await fetch(`http://127.0.0.1:${PORT}/ws?daemon=nope`);
  expect(res.status).toBe(400);
  expect(((await res.json()) as { error: string }).error).toContain("no daemon called 'nope'");
});
