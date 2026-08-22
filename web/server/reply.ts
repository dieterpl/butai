// The bridge's own replies. Everything a daemon says is passed through
// untouched; this is only for what the bridge itself has to answer.

import { Refused } from "./roster.ts";

export function json(status: number, body: unknown): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: { "Content-Type": "application/json" },
  });
}

/** A `Refused` as its reply. Anything else is a 500 with its message. */
export function refused(e: unknown): Response {
  if (e instanceof Refused) return json(e.status, { error: e.message });
  return json(500, { error: e instanceof Error ? e.message : String(e) });
}

export const NOT_FOUND = () => json(404, { error: "not found" });
