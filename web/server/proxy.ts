// One request against one daemon's HTTP-over-socket API.
//
// This file is the clearest argument for the port. `server.py` needed
// `butai_request`, `butai_open_stream`, `BodyReader` and a chunked-transfer
// decoder — about 150 lines — because Python's standard library has no HTTP
// client that speaks to an AF_UNIX socket. Bun's `fetch` takes a `unix` option,
// so the whole of it is the two functions below, and the streaming case is just
// "do not read the body to the end".

import type { DaemonRef } from "./roster.ts";

/** How long a single API round trip may take. */
const TIMEOUT_MS = 30_000;

export interface DaemonReply {
  status: number;
  headers: Headers;
  body: Uint8Array;
}

/**
 * One request/response against one daemon.
 *
 * The host in the URL is never resolved — `unix` decides where the connection
 * goes — but it has to be *there*, because the daemon reads the `Host` header
 * and a URL with no authority is not a URL. `localhost` is the conventional
 * placeholder.
 *
 * Binary-safe in both directions: file uploads ride this path, and so do
 * downloads, so the body is bytes and is never decoded on the way through.
 */
export async function butaiRequest(
  daemon: DaemonRef,
  method: string,
  path: string,
  body?: Uint8Array | undefined,
  ctype = "application/json",
  timeoutMs = TIMEOUT_MS,
): Promise<DaemonReply> {
  // Built key by key rather than as one literal: `exactOptionalPropertyTypes`
  // is on, and an explicit `body: undefined` is not the same as no `body` —
  // which is the point of the setting and worth keeping for the DTOs even
  // though it costs this small awkwardness at the two `fetch` calls.
  const init: BunFetchRequestInit = { unix: daemon.socket, method, signal: AbortSignal.timeout(timeoutMs) };
  if (body && body.byteLength) {
    init.headers = { "Content-Type": ctype };
    init.body = body as BodyInit;
  }
  const res = await fetch(`http://localhost${path}`, init);
  return { status: res.status, headers: res.headers, body: new Uint8Array(await res.arrayBuffer()) };
}

/** The same call, with the body left open — for `GET /v1/events`. */
export async function butaiStream(daemon: DaemonRef, path: string, signal?: AbortSignal): Promise<Response> {
  // No timeout: an event stream is *supposed* to stay open, and the whole point
  // of it is the long silences. It ends when the daemon closes the socket or the
  // browser goes away, and `signal` is how the second one gets through.
  const init: BunFetchRequestInit = { unix: daemon.socket };
  if (signal) init.signal = signal;
  return fetch(`http://localhost${path}`, init);
}

/** `butaiRequest`, with the body parsed as JSON. Throws what `fetch` throws. */
export async function butaiJson<T = unknown>(daemon: DaemonRef, method: string, path: string): Promise<T> {
  const r = await butaiRequest(daemon, method, path);
  return JSON.parse(new TextDecoder().decode(r.body) || "null") as T;
}
