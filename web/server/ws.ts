// One browser WebSocket, bridged to one framed butai socket connection.
//
//   browser text frame        == one ClientMsg JSON -> length-prefixed to the daemon
//   daemon length-prefixed    == one ServerMsg JSON -> browser text frame
//
// The bridge understands neither side's semantics; it only translates framing.
// **Which is why the daemon is chosen by `?daemon=<key>` on the URL and not by a
// qualified pane id inside the attach message**: reading the pane out of the
// payload would mean parsing a `ClientMsg` here, and this relay's whole contract
// is that it does not. So the URL says which machine, and the pane target
// carries the bare pane id that machine understands.
//
// `server.py` needed `ws_handshake`, `ws_send` and `ws_recv` — an RFC 6455
// implementation with its own SHA-1 accept digest and its own frame masking,
// about 120 lines. Bun.serve does the WebSocket; what is left is the daemon's
// own 4-byte length prefix, which nothing supplies for us.

import type { ServerWebSocket } from "bun";
import type { DaemonRef } from "./roster.ts";

export interface WsData {
  daemon: DaemonRef;
}

/** The daemon's framing: a 4-byte big-endian length, then that many bytes. */
const HEADER = 4;

/**
 * Per-connection state.
 *
 * The two hazards this exists for. **A stream is not a sequence of messages**:
 * `Bun.connect`'s `data` callback hands over whatever arrived, which may be half
 * a frame or three and a half, so frames are cut out of an accumulating buffer
 * rather than assumed to arrive whole. And **the browser can talk before the
 * daemon socket is open** — the upgrade completes immediately, the dial does
 * not — so early messages are queued rather than dropped, which is exactly the
 * window the client's `hello` lands in.
 */
class Bridge {
  private buf = new Uint8Array(0);
  private daemon: Bun.Socket<undefined> | null = null;
  private pending: Uint8Array[] = [];
  private closed = false;

  constructor(private readonly ws: ServerWebSocket<WsData>) {}

  async dial(ref: DaemonRef): Promise<void> {
    try {
      this.daemon = await Bun.connect({
        unix: ref.socket,
        socket: {
          data: (_s, chunk) => this.fromDaemon(chunk),
          close: () => this.close(),
          error: () => this.close(),
        },
      });
    } catch (e) {
      // Best-effort: tell the browser, then close.
      try {
        this.ws.send(JSON.stringify({ error: `butai socket: ${e instanceof Error ? e.message : String(e)}` }));
      } catch {
        /* the browser may already be gone */
      }
      this.close();
      return;
    }
    if (this.closed) {
      this.daemon.end();
      return;
    }
    for (const p of this.pending) this.writeFrame(p);
    this.pending = [];
  }

  /** A browser message: length-prefix it and pass it on, unread. */
  fromBrowser(msg: string | Uint8Array): void {
    const payload = typeof msg === "string" ? new TextEncoder().encode(msg) : msg;
    if (!this.daemon) {
      this.pending.push(payload);
      return;
    }
    this.writeFrame(payload);
  }

  private writeFrame(payload: Uint8Array): void {
    const out = new Uint8Array(HEADER + payload.byteLength);
    new DataView(out.buffer).setUint32(0, payload.byteLength, false); // big-endian
    out.set(payload, HEADER);
    this.daemon?.write(out);
  }

  /** Daemon bytes: cut whole frames out and forward each payload verbatim. */
  private fromDaemon(chunk: Uint8Array): void {
    // Append. Reallocating per chunk is fine here — a frame is one pane's
    // damage diff, not a file — and it keeps the extraction below obviously
    // correct, which matters more than the copy.
    const grown = new Uint8Array(this.buf.byteLength + chunk.byteLength);
    grown.set(this.buf);
    grown.set(chunk, this.buf.byteLength);
    this.buf = grown;

    for (;;) {
      if (this.buf.byteLength < HEADER) return;
      const n = new DataView(this.buf.buffer, this.buf.byteOffset, HEADER).getUint32(0, false);
      if (this.buf.byteLength < HEADER + n) return;
      const payload = this.buf.subarray(HEADER, HEADER + n);
      try {
        // A text frame, as the Python sent: the client reads `event.data` as a
        // string and `JSON.parse`s it. Sending the bytes would make it a binary
        // frame and hand the client a Blob instead.
        this.ws.send(new TextDecoder().decode(payload));
      } catch {
        this.close();
        return;
      }
      this.buf = this.buf.slice(HEADER + n);
    }
  }

  close(): void {
    if (this.closed) return;
    this.closed = true;
    try {
      this.daemon?.end();
    } catch {
      /* already gone */
    }
    try {
      this.ws.close();
    } catch {
      /* already gone */
    }
  }
}

const BRIDGES = new WeakMap<ServerWebSocket<WsData>, Bridge>();

export const websocket: Bun.WebSocketHandler<WsData> = {
  open(ws) {
    const b = new Bridge(ws);
    BRIDGES.set(ws, b);
    void b.dial(ws.data.daemon);
  },
  message(ws, msg) {
    BRIDGES.get(ws)?.fromBrowser(msg);
  },
  close(ws) {
    BRIDGES.get(ws)?.close();
  },
};
