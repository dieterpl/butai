// `GET /v1/events` on a Unix socket, relayed to the browser as Server-Sent
// Events at `/api/events`.
//
// Three kinds of record go down this stream, in this order:
//
//   `event: hello`     the bridge naming the daemon it is attached to
//   `event: snapshot`  that daemon's slice of `/api/state`
//   (unnamed)          the daemon's own records, byte for byte
//
// **Named records are the bridge's; unnamed records are the daemon's and are
// never parsed here.** That split is the contract: this relays framing exactly
// as `/ws` relays framing, and understands neither payload. Adding a seventh
// `ApiEvent` tag must not require a line in this file.
//
// **The order is load-bearing.** The daemon's stream carries no history — a
// subscriber sees only what happens after it subscribes — so the subscription is
// opened *first* and the snapshot is built after it. Anything that changes in
// between is already queued on the daemon socket and arrives after the snapshot,
// which makes it a repeat (harmless: every tag is a full snapshot of its
// subject) rather than a loss. Building the snapshot first would leave that
// window silently missing, which is the bug this ordering exists to prevent.
//
// **One daemon, one stream, and that is how fanning out works.** A client with
// several daemons opens one of these per daemon (`/api/events?daemon=<key>`)
// rather than multiplexing them into one record shape. The alternative —
// wrapping each daemon record in an envelope naming its source — would mean
// parsing and re-serialising every record here, which is the one thing this
// relay must not do.

import { butaiStream } from "./proxy.ts";
import { daemonSnapshot } from "./snapshot.ts";
import type { DaemonRef } from "./roster.ts";
import { json } from "./reply.ts";

/** One Server-Sent Event record. `name` undefined means the default `message`. */
function sse(name: string | undefined, data: string): string {
  // `JSON.stringify` never emits a literal newline, so one `data:` line is
  // always enough.
  return (name ? `event: ${name}\n` : "") + `data: ${data}\n\n`;
}

// How long the stream may sit silent before the bridge writes a comment record.
// Two jobs: it keeps an intermediary from timing the idle connection out, and
// writing something is the only way to find out that the browser has gone away
// when the daemon has nothing to say.
const KEEPALIVE_MS = 15_000;

export async function relayEvents(daemon: DaemonRef, lastEventId: string | null): Promise<Response> {
  const abort = new AbortController();
  let upstream: Response;
  try {
    upstream = await butaiStream(daemon, "/v1/events", abort.signal);
  } catch (e) {
    return json(502, { error: `butai socket: ${e instanceof Error ? e.message : String(e)}` });
  }

  const ctype = upstream.headers.get("content-type") ?? "";
  if (upstream.status !== 200 || !ctype.includes("text/event-stream")) {
    // A daemon too old to serve /v1/events 404s here. Answer with a plain JSON
    // error and not an empty event stream: the browser retries a dropped
    // *stream* forever but gives up on a reply that is not a 200
    // text/event-stream, and that difference is precisely the signal the client
    // needs to fall back to polling instead of waiting for a push that is never
    // coming.
    abort.abort();
    return json(upstream.status >= 400 ? upstream.status : 502, {
      error: "the daemon does not serve GET /v1/events",
      status: upstream.status,
      content_type: ctype,
    });
  }

  const enc = new TextEncoder();
  const body = new ReadableStream<Uint8Array>({
    async start(controller) {
      const send = (s: string) => controller.enqueue(enc.encode(s));
      let idle: ReturnType<typeof setInterval> | undefined;
      try {
        // `retry` is the browser's reconnect delay. 2s rather than the 3s
        // default: a reconnect costs one snapshot, and the client is blind
        // until it lands.
        send("retry: 2000\n\n");
        send(
          sse(
            "hello",
            JSON.stringify({
              // Which daemon this connection is attached to. Everything that
              // arrives below belongs to it, and the client qualifies the bare
              // ids in the daemon's own records with this key — attribution by
              // connection, not by envelope.
              daemon: daemon.key,
              label: daemon.label,
              primary: daemon.primary,
              socket: daemon.socket,
              // Echoed, not honoured. The daemon's stream has no cursor and no
              // history, so no id the bridge invented could be resumed from —
              // saying so is more use to a client than a number that promises a
              // replay nobody can perform.
              last_event_id: lastEventId,
              resumable: false,
            }),
          ),
        );
        send(sse("snapshot", JSON.stringify(await daemonSnapshot(daemon))));

        let quiet = true;
        idle = setInterval(() => {
          if (quiet) {
            try {
              send(": keepalive\n\n");
            } catch {
              /* the browser went away; the read loop below will notice */
            }
          }
          quiet = true;
        }, KEEPALIVE_MS);

        const reader = upstream.body!.getReader();
        for (;;) {
          const { done, value } = await reader.read();
          if (done) break;
          quiet = false;
          controller.enqueue(value); // the daemon's records, verbatim
        }
      } catch {
        /* browser gone, or daemon gone; either way this stream is over */
      } finally {
        if (idle) clearInterval(idle);
        abort.abort(); // drops us from the daemon's subscriber list
        try {
          controller.close();
        } catch {
          /* already closed */
        }
      }
    },
    cancel() {
      abort.abort();
    },
  });

  return new Response(body, {
    headers: {
      "Content-Type": "text/event-stream; charset=utf-8",
      "Cache-Control": "no-store, must-revalidate",
      // Tell an nginx in front not to buffer.
      "X-Accel-Buffering": "no",
    },
  });
}
