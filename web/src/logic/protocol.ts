// butai client protocol — pure message builders + keyboard mapping.
//
// The daemon's framed protocol is externally-tagged, snake_case JSON (see
// crates/butai-protocol/src/lib.rs). These helpers produce exactly those shapes;
// nothing here touches the DOM or the socket.
//
// The shapes themselves are not written out here: `ClientMsg`, `InputEvent`,
// `KeyCode` and the rest come from the generated bindings, so a builder that
// drifts from the Rust stops compiling instead of failing on the wire.

import type {
  AttachTarget,
  ClientMsg,
  InputEvent,
  KeyCode,
  KeyEvent,
  KeyMods,
  MouseButton,
  PaneId,
  ServerMsg,
} from "../protocol/generated/protocol.ts";

export const PROTO_VERSION = 1;

// This client's own build string, kept in step with the workspace version in
// Cargo.toml. check.py asserts the two are equal whenever the Rust source is
// alongside — a constant that drifts makes the mismatch banner below lie in the
// one situation it exists for.
export const CLIENT_VERSION = "1.0.0";

// The oldest daemon we will send `watch` to. It landed in 0.6 as an additive
// change, but a daemon older than that could not *decode* it and closed the
// connection — which presents as the stage blanking over and over, with nothing
// anywhere naming a version. `server_version` postdates that incident, so its
// absence means "older than any client able to look" and we re-dial instead.
export const MIN_SERVER_FOR_WATCH = "0.6.0";

// Compare two dotted version strings numerically. Anything after the numbers
// (`-rc1`, `+deadbeef`) is ignored: it does not order, and guessing at it would
// be worse than not looking.
export function cmpVersion(a: string, b: string): number {
  // Cut at the first character that is not part of the dotted number, so `rc1`
  // is dropped rather than becoming a fourth component that sorts *above* the
  // release it precedes.
  const parts = (v: string) => String(v).replace(/[^0-9.].*$/, "").split(".").map((n) => parseInt(n, 10) || 0);
  const [x, y] = [parts(a), parts(b)];
  for (let i = 0; i < Math.max(x.length, y.length); i++) {
    const d = (x[i] || 0) - (y[i] || 0);
    if (d) return d < 0 ? -1 : 1;
  }
  return 0;
}

// Is the daemon that sent us this `server_version` at least `min`? A daemon
// that sent none is not — see MIN_SERVER_FOR_WATCH.
export function daemonAtLeast(serverVersion: string | null | undefined, min: string): boolean {
  return typeof serverVersion === "string" && cmpVersion(serverVersion, min) >= 0;
}

/**
 * The payload of the daemon's `hello`, as this client is willing to read one.
 *
 * `Partial`, and deliberately: the whole point of [`helloProblem`] is a daemon
 * that is not the one this client was built against, so requiring the fields a
 * current daemon sends would type away the case being checked for.
 */
export type ServerHello = Partial<Extract<ServerMsg, { hello: unknown }>["hello"]>;

// What to tell the user about the daemon behind a server `hello`, or null when
// there is nothing to say.
//
// `proto_version` cannot carry this on its own: the versioning rule is that
// additive changes do not bump it, so a daemon and a client many releases apart
// both report 1 and the handshake sees nothing wrong. What the user sees instead
// is the *consequences* — commands the daemon has never heard of — which is
// exactly the bug report nobody can read. `server_version` is the daemon's own
// build string, and its absence is itself informative.
export function helloProblem(hello: ServerHello | null | undefined): string | null {
  if (!hello || typeof hello !== "object") return null;
  if (hello.proto_version !== PROTO_VERSION) {
    return `protocol mismatch: daemon speaks ${hello.proto_version}, this client speaks ${PROTO_VERSION}`;
  }
  const sv = hello.server_version;
  if (sv == null) return `daemon reports no version, so it predates ${CLIENT_VERSION} — restart it`;
  if (sv !== CLIENT_VERSION) return `daemon is ${sv}, client is ${CLIENT_VERSION} — restart it`;
  return null;
}

// A `hello` message — the first thing every attach sends.
//
// `target` says what to attach to: `paneTarget(id)` for one pane full-bleed
// (what the web client's stage does), or "default" for the most recent session,
// creating one if none exist — the whole workbench in our viewport.
export function helloMsg(cols: number, rows: number, target: AttachTarget = "default", cwd = "/"): ClientMsg {
  return {
    hello: {
      proto_version: PROTO_VERSION,
      encoding: "json",
      cols,
      rows,
      target,
      cwd,
    },
  };
}

// The attach target for a single pane.
export function paneTarget(pane: PaneId): AttachTarget {
  return { pane: { pane } };
}

export function resizeMsg(cols: number, rows: number): ClientMsg {
  return { resize: { cols, rows } };
}

export function pasteMsg(text: string): ClientMsg {
  return { input: { paste: text } };
}

// Largest file `put_file` accepts, decoded — mirrors MAX_PUT_FILE_BYTES in
// crates/butai-protocol/src/lib.rs. Checked here as well so a phone's camera
// roll fails immediately instead of after uploading 40 MB to be refused.
export const MAX_PUT_FILE_BYTES = 8 * 1024 * 1024;

// Hand a file to the daemon: it writes it to the workspace's scratch directory
// and pastes the absolute path into the pane, which is what an agent CLI wants.
// The daemon answers with `file_put`.
//
// A command rather than the `POST .../upload` REST route on purpose — that one
// writes into the *project* and the file shows up in the changes rail, which is
// right for a file you meant to add and wrong for a pasted screenshot.
export function putFileMsg(name: string, bytes: Uint8Array): ClientMsg {
  return { command: { put_file: { name, data: b64(bytes) } } };
}

// Bytes -> standard base64. The protocol carries file data as a string so a
// JSON and a MessagePack client send the identical structure.
//
// Chunked because spreading a whole file into the argument list blows the stack
// somewhere around a hundred thousand bytes — well under the sizes this is for.
function b64(bytes: Uint8Array): string {
  let s = "";
  for (let i = 0; i < bytes.length; i += 0x8000) {
    s += String.fromCharCode(...bytes.subarray(i, i + 0x8000));
  }
  return btoa(s);
}

/** The three `InputEvent` arms a pointer produces. */
export type MouseKind = "mouse_down" | "mouse_drag" | "mouse_up";

export function mouseMsg(
  kind: MouseKind,
  x: number,
  y: number,
  alt = false,
  button: MouseButton = "left",
): ClientMsg {
  // kind: "mouse_down" | "mouse_drag" | "mouse_up". `alt` forces a butai text
  // selection even over an app that grabbed the mouse (mouse_up carries no alt).
  const payload: { x: number; y: number; alt?: boolean; button?: MouseButton } =
    kind === "mouse_up" ? { x, y } : { x, y, alt };
  // `button` rides on mouse_down only, and only when it is not the default: the
  // daemon skips it when left, so a left click stays byte-identical to what
  // clients sent before right-click existed. mouse_drag and mouse_up carry none
  // by design — only the left button drags, so a right press has no release.
  if (kind === "mouse_down" && button !== "left") payload.button = button;
  // The arm is chosen at runtime, so the key is computed and the compiler cannot
  // line it up with `InputEvent`'s variants itself. `payload` above is the shape
  // each of the three carries.
  return { input: { [kind]: payload } as InputEvent };
}

// A browser MouseEvent.button -> the protocol's MouseButton, or null for one it
// has no name for (middle, back, forward) and which must not be sent.
export function mouseButton(n: number): MouseButton | null {
  return n === 0 ? "left" : n === 2 ? "right" : null;
}

export function scrollMsg(up: boolean, x: number, y: number): ClientMsg {
  return { input: { [up ? "scroll_up" : "scroll_down"]: { x, y } } as InputEvent };
}

// Re-point a `pane` connection at another pane, without reconnecting. Answered
// with a full frame, exactly like a fresh attach — so clear the screen before
// applying it. Refused (with `error`, and the old pane still streaming) on a
// connection that is not showing a pane, or for a pane that no longer exists.
export function watchMsg(pane: PaneId): ClientMsg {
  return { watch: { pane } };
}

// Something only the client could know, put wherever the daemon shows `error`.
// The clipboard is the case it was added for: "no image on the clipboard" is
// known by the side that looked, and a request that does nothing at all is
// indistinguishable from a broken one.
export function noticeMsg(text: string): ClientMsg {
  return { notice: String(text).slice(0, MAX_NOTICE_CHARS) };
}

// Longest notice the daemon will show — it truncates past this and appends an
// ellipsis, so we cut it here and keep the message ours.
export const MAX_NOTICE_CHARS = 200;

// Unit variants ride the wire as bare strings, not one-key objects.
export function detachMsg(): ClientMsg {
  return "detach";
}

// Named (non-character) keys → the protocol's KeyCode string variants.
const NAMED: Readonly<Record<string, KeyCode>> = {
  Enter: "enter",
  Escape: "esc",
  Backspace: "backspace",
  Tab: "tab",
  ArrowLeft: "left",
  ArrowRight: "right",
  ArrowUp: "up",
  ArrowDown: "down",
  Home: "home",
  End: "end",
  PageUp: "page_up",
  PageDown: "page_down",
  Delete: "delete",
  Insert: "insert",
};

// Map a browser KeyboardEvent to an `{input:{key:{code,mods}}}` message, or
// null for keys the protocol does not carry (bare modifiers, dead keys, etc.).
export function keyMsg(e: KeyboardEvent): ClientMsg | null {
  const mods: KeyMods = {};
  if (e.ctrlKey) mods.ctrl = true;
  if (e.altKey) mods.alt = true;
  if (e.shiftKey) mods.shift = true;

  let code: KeyCode | null = null;
  const k = e.key;
  const named = NAMED[k];

  if (k === "Tab" && e.shiftKey) {
    code = "back_tab";
    delete mods.shift; // back_tab already encodes the shift
  } else if (named) {
    code = named;
  } else if (/^F\d{1,2}$/.test(k)) {
    code = { f: parseInt(k.slice(1), 10) };
  } else if (k.length === 1) {
    // A single printable character (grapheme). Shift is already reflected in
    // the character itself, so don't also send the shift modifier.
    code = { char: k };
    delete mods.shift;
  } else {
    return null; // "Shift", "Control", "Meta", "Dead", ...
  }

  const key: KeyEvent = { code };
  if (Object.keys(mods).length) key.mods = mods;
  return { input: { key } };
}

// True when we should let the browser handle the event natively (never send it).
// Deliberately narrow: Ctrl-C / Ctrl-D etc. MUST reach the pane (terminal
// interrupt/EOF). The daemon does its own copy from server-side selection and
// pushes it via `set_clipboard`, so we don't hijack Ctrl-C for copy. We only
// let through paste (so the `paste` event delivers the text), page reload, and
// the devtools keys.
export function isPassthrough(e: KeyboardEvent): boolean {
  const k = (e.key || "").toLowerCase();
  if ((e.ctrlKey || e.metaKey) && k === "v") return true; // native paste event
  if (e.key === "F5" || ((e.ctrlKey || e.metaKey) && k === "r")) return true;
  if (e.key === "F12") return true;
  if (e.ctrlKey && e.shiftKey && (k === "i" || k === "j" || k === "c")) return true;
  return false;
}
