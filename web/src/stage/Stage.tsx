// <Stage> — the live terminal. Wraps a [`Screen`] (the cell-grid renderer) and
// owns one WebSocket to the bridge's /ws, attached to a single pane via the
// framed protocol's `Pane` target. This is the ONLY streamed part of the UI; the
// rest of the chrome is React fed by the REST API. Change the `pane` prop to
// re-point it; it reconnects and streams just that pane.
//
// `pane` is a **qualified pane id** — `<daemon>:<pane>`. A socket reaches one
// daemon, so the daemon half decides which socket to dial (`/ws?daemon=<key>`)
// and the pane half is what goes in the attach target. Two consequences worth
// naming: switching panes *within* a machine still re-points the live connection
// with `watch`, and switching *between* machines has to re-dial, because there
// is no message that moves a socket to another host.
//
// Ported from `web/butai-stage.js`. The connection is a plain class rather than
// hooks, for the same reason the renderer is: it is a state machine with a
// refusal cache, a backoff and an in-flight `watch`, none of which is React's
// business and all of which would be re-created on a re-render. React owns the
// <canvas>, the empty state and the status pill; everything else is the port.
//
// **Frames never reach React.** A PTY at full rate is thousands of updates a
// second and each one goes straight to the canvas. The only thing here that
// moves React state is the status pill, which changes when the socket does.

import { useEffect, useImperativeHandle, useRef, useState } from "react";
import type { Ref } from "react";
import { Screen } from "./Screen.ts";
import type { TermTheme } from "../logic/palette.ts";
import { daemonOf, localId } from "../logic/events.ts";
import type { Qid } from "../logic/events.ts";
import {
  helloMsg, paneTarget, watchMsg, noticeMsg, detachMsg, resizeMsg,
  helloProblem, daemonAtLeast, MIN_SERVER_FOR_WATCH,
} from "../logic/protocol.ts";
import type { ClientMsg, ServerMsg } from "../protocol/generated/protocol.ts";

// How long the stage remembers that the daemon refused a pane. Long enough to
// cover a snapshot that is stale about a pane that just exited (which is what
// makes the client ask again at all), short enough that a daemon restarted
// under an open tab — new panes, ids beginning again — is not locked out.
const REFUSAL_TTL_MS = 60000;

/** The status pill's contents, or `null` when it is hidden. */
export interface StageStatus {
  text: string;
  /** Red: the connection is down or the daemon said no. */
  down: boolean;
}

/** The daemon behind the socket, as its `hello` described it. */
export interface DaemonVersion {
  version: string | null;
  /** What to tell the user about it, or null when there is nothing to say. */
  problem: string | null;
}

/** A pane the daemon does not have. Drop it from the selection. */
export interface PaneRefusal {
  pane: Qid | null;
  error: string;
}

/** What the stage tells whoever is above it. All optional; all rare. */
export interface StageEvents {
  onBell?: () => void;
  onDaemonVersion?: (info: DaemonVersion) => void;
  onPaneRefused?: (info: PaneRefusal) => void;
}

export interface StageProps extends StageEvents {
  /** The qualified pane id to stream, or null for the empty state. */
  pane: Qid | null;
  /**
   * What a `"default"` cell resolves to — `termColors(palette)`.
   *
   * A prop rather than a read of `--term-bg` off the page: the canvas cannot
   * inherit a custom property, so somebody has to push the palette in, and a
   * prop is the one place a re-render can see it change.
   */
  theme: TermTheme;
  /** Cell size in CSS pixels. Omitted leaves the renderer's own default. */
  fontPx?: number;
  className?: string;
  ref?: Ref<StageHandle>;
}

/** The imperative surface `keys.ts` drives the stage through. */
export interface StageHandle {
  focusTerminal(): void;
  blurTerminal(): void;
  /**
   * Put one protocol message on the wire. The prefix pressed twice is the only
   * caller: it is a key the workbench claimed and then decided belongs to the
   * program after all, which is exactly what tmux's doubled prefix means.
   *
   * `null` is in the signature because `keyMsg` can answer with one and the
   * caller hands its answer straight over — see `keys.ts`'s `StageEl`.
   */
  send(msg: ClientMsg | null): void;
  /**
   * Force a full repaint and reclaim the pane's size for this viewer. The
   * corner button calls it; a key binding can too, without learning how it
   * works — see [`StageConn.redraw`].
   */
  redraw(): void;
}

/** What the connection needs from the component around it. */
interface ConnHooks {
  /** Draw (or hide) the status pill. */
  onStatus: (status: StageStatus | null) => void;
  /**
   * The component's current callbacks, read at call time. Props change identity
   * on every render and the connection outlives all of them.
   */
  events: () => StageEvents;
}

/**
 * The socket, the refusal cache and the reconnect backoff.
 *
 * A transliteration of `butai-stage.js` minus its DOM: `_setStatus`/`_flash`
 * call a hook instead of writing to a <span>, and the four CustomEvents it
 * dispatched are four optional callbacks.
 */
class StageConn {
  ws: WebSocket | null = null;
  pane: Qid | null = null;
  closedByUs = false;
  /**
   * The daemon's build string, from its hello. null until we have one, which is
   * also the gate on `watch` — see setPane().
   */
  serverVersion: string | null = null;
  /** Which daemon the current socket reaches. Set by `_connect`. */
  daemonKey: string | null = null;

  private readonly screen: Screen;
  private readonly hooks: ConnHooks;
  private _reconnect: ReturnType<typeof setTimeout> | undefined;
  private _flashTimer: ReturnType<typeof setTimeout> | undefined;
  /**
   * The pane we were streaming when the outstanding `watch` was sent, so a
   * refusal can put us back on it. null when nothing is outstanding.
   */
  private _preWatch: Qid | null = null;
  /**
   * Panes the daemon has already told us it does not have, and when it said so.
   * Kept across re-dials on purpose — a refused attach *causes* a re-dial, so
   * forgetting there would forget the thing we just learned — and expired after
   * REFUSAL_TTL_MS, because a daemon restarted under us begins its pane ids
   * again and an id we refuse forever would then be one you cannot open without
   * reloading the page.
   *
   * Keyed by the *qualified* pane id, which is the whole reason ids are
   * qualified: one machine refusing its pane 5 must not lock you out of another
   * machine's pane 5, and a bare-int key would do exactly that.
   */
  private readonly _refused = new Map<Qid, number>();
  /**
   * Has this connection produced a frame? An `error` before the first one is the
   * attach itself being refused; after it, it is an answer to something we asked
   * for. Same message, opposite meaning.
   */
  private _gotFrame = false;
  /** Successive failed dials, for the backoff. Reset by a frame. */
  private _tries = 0;
  /** What the pill currently says — `_flash`'s "is this still mine?" guard. */
  private _statusText: string | null = null;

  constructor(screen: Screen, hooks: ConnHooks) {
    this.screen = screen;
    this.hooks = hooks;
  }

  /** The old `disconnectedCallback`. */
  destroy(): void {
    clearTimeout(this._reconnect);
    clearTimeout(this._flashTimer);
    this._sayGoodbye();
  }

  // Close the connection the way the protocol says to. `detached` comes back
  // and the daemon closes; `closedByUs` keeps the reconnect timer out of it.
  _sayGoodbye(): void {
    if (!this.ws) return;
    this.closedByUs = true;
    if (this.ws.readyState === WebSocket.OPEN) this._send(detachMsg());
    this.ws.close();
  }

  setPane(pane: Qid | null): void {
    // Already on it, or already asked for it. Whoever calls this calls it
    // whenever the rails redraw, not only when you click — so the ordinary case
    // is that nothing changed.
    if (pane === this.pane) return;
    // The daemon has already refused this one. Re-asking costs a round trip
    // whose answer we are holding, and — before this — a stale snapshot naming
    // a pane that has just exited made the client ask again on every refresh:
    // as a `watch` when the socket survived the refusal, and as a whole new
    // WebSocket when it did not. Say no here and tell the caller again, so it
    // drops the selection instead of coming back with it.
    if (pane != null && this._isRefused(pane)) {
      this._refuse(pane, "no pane " + pane);
      return;
    }
    const prev = this.pane;
    this.pane = pane;
    // Re-point the live connection instead of dialling a new one. The daemon
    // grew `watch` for this exact call site: tearing the socket down and
    // reconnecting is a visible stall on any link with latency, for what is
    // bookkeeping on its side. Reconnect stays the fallback — for a socket that
    // is actually gone, for a daemon too old to have `watch` at all (which would
    // answer an unknown message by closing the connection), and for a pane on a
    // *different machine*, which this socket cannot reach at all.
    const local = pane == null ? null : localId(pane);
    if (pane != null && local != null && this._canWatch(pane)) {
      if (this._preWatch === null) this._preWatch = prev;
      // A watch is answered with a full frame "exactly as if you had attached",
      // so clear first, the way an attach does.
      this.screen.clear();
      this.flash("switching…", false, 1500);
      this._send(watchMsg(local));
      return;
    }
    this._connect();
  }

  // Can this pane change ride the connection we already have?
  //
  // Only if it is on the same daemon. `watch` re-points a connection *within*
  // one daemon; the connection itself is one socket to one machine, so a pane
  // on another one is a re-dial however new the daemon is. Getting this wrong
  // would send B's pane id down A's socket, where it names one of A's panes and
  // is answered with a perfectly good screen belonging to the wrong machine.
  private _canWatch(pane: Qid): boolean {
    return (
      !!this.ws &&
      this.ws.readyState === WebSocket.OPEN &&
      daemonOf(pane) === this.daemonKey &&
      daemonAtLeast(this.serverVersion, MIN_SERVER_FOR_WATCH)
    );
  }

  // Put one protocol message on the wire.
  send(msg: ClientMsg | null): void {
    this._send(msg);
  }

  /**
   * Repaint the pane from scratch, and take its size back.
   *
   * A `resize` at the size we already are, which sounds like a no-op and is
   * not. Two things happen on the other side: the daemon drops this client's
   * diff baseline, so the next frame it sends is a **full** one rather than a
   * patch against whatever we think we are showing; and it points the PTY at
   * *this* viewer's dimensions.
   *
   * The second half is the reason there is a button at all. A pane holds one
   * size and the last client to attach, resize or type wins it, so a second
   * viewer opening the same pane at another size leaves this one drawing the
   * program's screen in a corner of its own frame — with no message saying so,
   * because the protocol has none. Until now the only way back was to type
   * something into the pane, which is a poor answer when the pane belongs to an
   * agent mid-turn. The PTY resize itself no-ops when the size already matches,
   * so on a stage nobody is fighting over this costs exactly one frame.
   *
   * A socket that is not open cannot be told anything, so a redraw asked for
   * while the connection is down skips the backoff and re-dials instead — which
   * is the other thing someone hitting this button means by it.
   */
  redraw(): void {
    if (this.pane == null) return;
    if (this.ws && this.ws.readyState === WebSocket.OPEN) {
      this.flash("redrawing…", false, 1200);
      this._send(resizeMsg(this.screen.cols, this.screen.rows));
      return;
    }
    this._connect();
  }

  private _connect(): void {
    clearTimeout(this._reconnect);
    // Re-dialling is a deliberate teardown of the old socket, so it gets the
    // same goodbye a closing tab does.
    this._sayGoodbye();
    this.closedByUs = false;
    // Everything below belongs to the socket we just dropped.
    this.serverVersion = null;
    this._preWatch = null;
    this._gotFrame = false;
    this.daemonKey = daemonOf(this.pane);

    const pane = this.pane;
    if (pane == null) {
      // The empty state is the component's: it draws it from the same `pane`
      // that got us here. All this has to do is take the pill down.
      this._hideStatus();
      return;
    }
    const local = localId(pane);
    if (local == null) {
      // An id with no `<daemon>:` half has no pane number to put on the wire.
      // The JS sent `{pane: null}` and let the daemon refuse it; there is
      // nothing to learn from the round trip, so refuse it here — same outcome,
      // one fewer dial. (It is the caller's bug either way: `localId` answers
      // null only for an id that was never qualified.)
      this._refuse(pane, "malformed pane id " + pane);
      this._setStatus("malformed pane id " + pane, true);
      return;
    }
    this.screen.clear();
    this._setStatus("connecting…", false);

    const proto = location.protocol === "https:" ? "wss" : "ws";
    // The daemon is named on the URL and not inside the attach message: the
    // bridge relays framing and never reads a payload, so the only place it can
    // learn which socket to open is the request line.
    const ws = new WebSocket(
      `${proto}://${location.host}/ws?daemon=${encodeURIComponent(this.daemonKey || "")}`,
    );
    this.ws = ws;

    ws.onopen = () => {
      const cols = this.screen.cols || 100;
      const rows = this.screen.rows || 30;
      // Attach to just this pane, full-bleed at our size. Built by protocol.ts
      // like every other message we send — there used to be a second, inline
      // copy of the hello shape here, which meant two places to change and one
      // of them always wrong. The daemon's own pane id goes on the wire; the
      // daemon has never heard of the key in front of it.
      this._send(helloMsg(cols, rows, paneTarget(local)));
    };
    ws.onmessage = (ev: MessageEvent) => {
      // The bridge relays text frames. Anything else would be a binary encoding
      // this client never asks for; the JS handed it to `JSON.parse` and let it
      // throw into the same `return`.
      if (typeof ev.data === "string") this._onMessage(ev.data);
    };
    ws.onclose = () => {
      if (this.closedByUs || this.pane !== pane) return;
      // The daemon closed on us because it has no such pane. Dialling again is
      // asking a question we are holding the answer to; wait for someone to
      // choose a different one.
      if (this._isRefused(pane)) {
        this._setStatus("no pane " + pane, true);
        return;
      }
      // Back off. A dial that keeps failing used to re-dial once a second
      // forever — a new WebSocket, a new framed connection and a new attach
      // attempt per second for as long as the tab stayed open. The frame that
      // means it worked resets this.
      const wait = Math.min(1000 * Math.pow(2, this._tries++), 10000);
      this._setStatus("reconnecting…", true);
      this._reconnect = setTimeout(() => { if (this.pane === pane) this._connect(); }, wait);
    };
    ws.onerror = () => {};
  }

  private _onMessage(data: string): void {
    let msg: ServerMsg;
    // The daemon's messages are `ServerMsg` by contract; this is the one place
    // the contract is asserted rather than checked, exactly as the JS had it.
    try { msg = JSON.parse(data) as ServerMsg; } catch { return; }
    if (msg === "bell") { this.hooks.events().onBell?.(); return; }
    if (msg === "ok") return;
    if (msg === "read_clipboard_image") {
      // The daemon's half of `paste_image`. A clipboard belongs to the machine
      // the *client* runs on — over `ssh host butai proxy` that is not even the
      // daemon's machine — so it asks us to look. We answer with `put_file`, or
      // with `notice` when there is nothing to send, which the daemon puts
      // wherever it puts its own errors.
      void this.screen.readClipboardImage().then((problem) => {
        if (problem) this._send(noticeMsg(problem));
      });
      return;
    }
    if ("frame" in msg) {
      // A full frame is what a successful `watch` answers with, so it also
      // retires the refusal path below. A stale diff for the pane we just left
      // can still be in flight — harmless, the socket is ordered — which is why
      // only a full frame counts.
      if (msg.frame.full) this._preWatch = null;
      this._gotFrame = true;
      this._tries = 0;
      this.screen.applyFrame(msg.frame);
      this._hideStatus();
    } else if ("hello" in msg) {
      this.serverVersion = msg.hello.server_version ?? null;
      const problem = helloProblem(msg.hello);
      this.hooks.events().onDaemonVersion?.({ version: this.serverVersion, problem });
      // Through `flash` for its "is this still mine?" guard: a bare timeout here
      // would hide whatever an error or a reconnect put in the pill inside the
      // 400ms window. Short, because "live" is only a handshake receipt — a
      // version problem is not, so it stays up ten times as long.
      if (problem) this.flash(problem, true, 8000);
      else this.flash("live", false, 400);
    } else if ("set_clipboard" in msg) {
      navigator.clipboard?.writeText(msg.set_clipboard).catch(() => {});
    } else if ("file_put" in msg) {
      // The path is already in the pane; say where it went, because the file
      // itself is somewhere the user never chose.
      this.flash(`pasted ${msg.file_put.path.split("/").pop()}`);
    } else if ("detached" in msg) {
      this._setStatus(`closed: ${msg.detached.reason}`, true);
    } else if ("error" in msg) {
      if (this._preWatch !== null) {
        // A refused `watch` leaves the daemon streaming what it had, so put our
        // own idea of the pane back rather than claiming one we are not being
        // sent. A pane can exit between the click and the daemon reading the
        // message, so this is ordinary rather than a client bug — and losing the
        // pane you already had would be the wrong answer to it. Whoever chose
        // the pane is told, so it can drop a selection that no longer exists
        // instead of asking again on the next refresh.
        const refused = this.pane;
        this.pane = this._preWatch;
        this._preWatch = null;
        this._refuse(refused, msg.error);
      } else if (!this._gotFrame) {
        // An error before this connection has drawn anything is the *attach*
        // being refused, not an answer to something we asked for — same message,
        // opposite meaning, and only the frame count separates them. It has to
        // be reported the same way a refused `watch` is: without this the stage
        // sat on a pane that does not exist, the daemon closed the socket, and
        // the reconnect above dialled a fresh one every second for as long as
        // the tab stayed open. Measured: 18 WebSockets in 15 seconds.
        this._refuse(this.pane, msg.error);
      }
      this._setStatus(msg.error, true);
    }
  }

  // Record a refusal and tell whoever chose the pane. `pane` stays out of
  // `this.pane` when a `watch` was refused — that already holds whatever we are
  // really being sent — and stays *in* it when the attach was, because there is
  // nothing else to hold and the caller is about to choose again.
  private _refuse(pane: Qid | null, error: string): void {
    if (pane != null) this._refused.set(pane, Date.now());
    this.hooks.events().onPaneRefused?.({ pane, error });
  }

  private _isRefused(pane: Qid): boolean {
    const at = this._refused.get(pane);
    if (at == null) return false;
    if (Date.now() - at < REFUSAL_TTL_MS) return true;
    this._refused.delete(pane);
    return false;
  }

  private _send(msg: ClientMsg | null): void {
    if (this.ws && this.ws.readyState === WebSocket.OPEN) this.ws.send(JSON.stringify(msg));
  }

  private _setStatus(text: string, down: boolean): void {
    this._statusText = text;
    this.hooks.onStatus({ text, down });
  }

  private _hideStatus(): void {
    this._statusText = null;
    this.hooks.onStatus(null);
  }

  // Borrow the status pill for a message about something the user just did.
  // Only clears itself if nothing else has claimed the pill since — otherwise
  // a socket that dropped mid-flash would lose its "reconnecting…".
  //
  // Public because the screen's `notice` lands here: a paste or drop it couldn't
  // complete (too large, unreadable), which is local by definition — nothing
  // asked for it, so nobody is waiting on an answer. The daemon-initiated case
  // goes back over the wire; see the `read_clipboard_image` arm.
  flash(text: string, bad = false, ms = 2500): void {
    clearTimeout(this._flashTimer);
    this._setStatus(text, bad);
    this._flashTimer = setTimeout(() => {
      if (this._statusText === text) this._hideStatus();
    }, ms);
  }
}

export function Stage({ pane, theme, fontPx, className, ref, ...events }: StageProps) {
  const canvasRef = useRef<HTMLCanvasElement | null>(null);
  const screenRef = useRef<Screen | null>(null);
  const connRef = useRef<StageConn | null>(null);
  const [status, setStatus] = useState<StageStatus | null>(null);

  // The renderer and the connection outlive every render; the props they need
  // are read through this at call time, so a new callback identity does not tear
  // the socket down.
  const live = useRef({ theme, events });
  live.current = { theme, events };

  useEffect(() => {
    const canvas = canvasRef.current;
    if (!canvas) return undefined;
    const screen = new Screen(canvas, {
      // `connRef` is filled in on the next line; the lookup is deferred to call
      // time because the screen emits its first `resize` 60ms from now.
      onMessage: (msg) => connRef.current?.send(msg),
      getTheme: () => live.current.theme,
      onNotice: (text) => connRef.current?.flash(text, true),
      ...(fontPx != null ? { fontPx } : {}),
    });
    const conn = new StageConn(screen, {
      onStatus: setStatus,
      events: () => live.current.events,
    });
    screenRef.current = screen;
    connRef.current = conn;
    // A closing tab should say goodbye rather than vanish: `detach` is the
    // protocol's own teardown, and it lets the daemon drop the client's
    // bookkeeping now instead of when the socket eventually reads EOF.
    const onPageHide = () => conn._sayGoodbye();
    window.addEventListener("pagehide", onPageHide);
    return () => {
      window.removeEventListener("pagehide", onPageHide);
      conn.destroy();
      screen.destroy();
      connRef.current = null;
      screenRef.current = null;
    };
    // Mount-only: `fontPx` is the renderer's starting size and its own effect
    // below carries every later value.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  useEffect(() => {
    connRef.current?.setPane(pane);
  }, [pane]);

  // A canvas cannot inherit a custom property, so a palette change has to be
  // pushed in. Keyed on the two colours rather than the object, or a parent that
  // builds `{fg, bg}` inline would repaint the grid on every render.
  useEffect(() => {
    screenRef.current?.refreshTheme();
  }, [theme.fg, theme.bg]);

  useEffect(() => {
    if (fontPx != null) screenRef.current?.setFontPx(fontPx);
  }, [fontPx]);

  useImperativeHandle(ref, (): StageHandle => ({
    focusTerminal() {
      if (connRef.current?.pane != null) screenRef.current?.focus();
    },
    // Take the keyboard *off* the pane without closing anything. The pane is
    // still live and still streaming; it is simply not listening, and the screen
    // draws a hollow cursor to say so — a solid cursor that ignores you is the
    // worst of both.
    blurTerminal() {
      screenRef.current?.blur();
    },
    send(msg) {
      connRef.current?.send(msg);
    },
    redraw() {
      connRef.current?.redraw();
    },
  }), []);

  return (
    <div
      className={
        "relative h-full bg-term-bg " +
        "focus-within:shadow-[inset_0_0_0_1px_var(--focus)] " +
        "[&.dropping]:shadow-[inset_0_0_0_2px_var(--accent)]" +
        (className ? " " + className : "")
      }
    >
      {/*
        The canvas is the whole renderer. It is hidden rather than unmounted when
        there is no pane, so `Screen` keeps its grid, its listeners and the
        off-screen paste sink it appended here — and so this element list never
        changes length, which is what keeps React and that sink out of each
        other's way.
      */}
      <canvas ref={canvasRef} className="absolute inset-0 block h-full w-full" hidden={pane == null} />
      <div
        className="pointer-events-none absolute inset-0 flex items-center justify-center p-5 text-center font-mono text-13 text-dim"
        hidden={pane != null}
      >
        Select an agent or process to open its terminal.
      </div>
      {/*
        The corner. One overlay for both the status pill and the redraw button
        so they cannot collide: the pill comes and goes, the button stays as
        long as there is a pane, and they share a row rather than each claiming
        the same corner absolutely.

        `pointer-events-none` on the strip, restored on the button alone —
        everything else here must let a click through to the canvas underneath,
        which is the whole terminal.
      */}
      <div
        className="pointer-events-none absolute inset-0 flex items-start justify-end gap-1 p-5"
        hidden={pane == null}
      >
        <span
          className={
            "m-2 rounded-md bg-black/40 px-2 py-0.5 font-mono text-11 " +
            (status?.down ? "text-bad" : "text-dim")
          }
          hidden={!status}
        >
          {status?.text ?? ""}
        </span>
        <button
          type="button"
          // Deliberately quiet: a control on top of a terminal is in the way of
          // the thing you came to read, so it sits at the dimmest step until
          // you go looking for it.
          className={
            "pointer-events-auto m-2 rounded-md bg-black/40 px-1.5 py-0.5 font-mono text-11 " +
            "text-dim opacity-40 transition-opacity hover:opacity-100 focus-visible:opacity-100 " +
            "focus-visible:outline focus-visible:outline-1 focus-visible:outline-[var(--focus)]"
          }
          title="Redraw — repaint this pane and take its size back (a second viewer can have claimed it)"
          aria-label="Redraw the terminal"
          onClick={() => {
            connRef.current?.redraw();
            // The click took the keyboard off the pane to give it to a button
            // nobody wants to keep. Hand it straight back, or the next thing
            // typed goes nowhere.
            screenRef.current?.focus();
          }}
        >
          {/*
            The word, not a glyph. The obvious icon for this is ⟳, and the
            PROCESSES rail two columns away already spends it on *restart* —
            which kills the program and starts it again. Two controls one row
            apart, one of them destructive, must not wear the same picture.
          */}
          redraw
        </button>
      </div>
    </div>
  );
}
