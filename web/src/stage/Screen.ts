// Screen — a faithful cell-grid renderer for the butai framed protocol.
//
// This renders the STAGE: one pane the daemon draws full-bleed into a styled
// cell grid, shipped as `frame` damage diffs. This class paints that grid onto
// a DPI-aware <canvas>, and turns keyboard / mouse / paste / wheel / resize into
// protocol messages. It knows nothing about workspaces or git — it just mirrors
// one pane's cells and forwards input, so the stage is byte-for-byte the
// terminal. (The same renderer would faithfully paint a whole-workbench session
// stream too; the client just doesn't use it that way anymore.)
//
// Transliterated from `web/butai-screen.js`, which is a custom element with a
// shadow root. This is a plain class and `Stage.tsx` owns the <canvas>; the one
// structural change that follows from dropping the shadow DOM is `_hasFocus`,
// which no longer has three shadow roots to walk. Everything else — the damage
// buffer, the three-pass render, the 0.5px background bleed, the 0.76 baseline,
// the rAF coalescing, the cursor pixel-stash, the blink — is the same code.
//
// ## What the caller has to provide
//
// The canvas is the drawing surface *and* the element the grid is sized from,
// so its CSS size must not depend on its backing store: `width:100%;
// height:100%` inside a sized box, exactly as `butai-screen.js`'s shadow
// stylesheet had it. A canvas laid out by its `width`/`height` attributes would
// grow every time `_onResize` wrote them.
//
// The canvas's parent element is the light-DOM stand-in for the old shadow
// host: input listeners bind there (so keys reach us whether the focused node is
// the off-screen sink or the canvas itself), the off-screen sink is appended
// there, and the `dropping` class lands there.

import { ScreenLinks } from "../logic/links.ts";
import { resolveColor } from "../logic/palette.ts";
import type { TermTheme } from "../logic/palette.ts";
import {
  keyMsg, isPassthrough, pasteMsg, mouseMsg, mouseButton, scrollMsg, resizeMsg,
  putFileMsg, MAX_PUT_FILE_BYTES,
} from "../logic/protocol.ts";
import type { ClientMsg, Color, CursorShape, FrameUpdate, Mods } from "../protocol/generated/protocol.ts";

const DEFAULT_FONT_PX = 15;

/**
 * One cell as the buffer holds it.
 *
 * The wire's [`Cell`] has `fg`, `bg` and `mods` optional; the buffer holds them
 * resolved, because `applyFrame` fills a default in as it writes and the render
 * loop must not branch on absence twice. `ch` keeps the wire's convention
 * exactly: `""` is the trailing half of a wide glyph and is *not* a space.
 */
interface BufCell {
  ch: string;
  fg: Color;
  bg: Color;
  mods: Mods | null;
}

/** What a `Screen` needs from whoever put the canvas on the page. */
export interface ScreenOptions {
  /**
   * Every protocol message this screen produces — keys, paste, mouse, scroll,
   * resize, `put_file`. The old element dispatched these as a `send`
   * CustomEvent and the stage relayed them to the socket; this is that edge.
   */
  onMessage: (msg: ClientMsg) => void;
  /**
   * The pane's default foreground and background — what a `"default"` cell
   * resolves to. Called on construction and again on every `refreshTheme`,
   * rather than read once: a canvas does not inherit a CSS variable the way the
   * rest of the chrome does, so a palette change has to be pushed in.
   */
  getTheme: () => TermTheme;
  /**
   * Something the user asked for that couldn't be done (a file too large to
   * send, a clipboard that would not be read). The old element raised a
   * `notice` CustomEvent; the stage draws it. A screen with no reporter just
   * doesn't say.
   */
  onNotice?: (text: string) => void;
  /**
   * A `preview` screen is one you look at: no pane behind it, nothing to send,
   * and — the part that matters — no tab stop and no grab for the keyboard. The
   * SETTINGS page draws one to show what a palette does, and a sample terminal
   * that takes the caret out of the list you are walking is worse than no
   * sample at all.
   */
  preview?: boolean;
  /** Starting font size in CSS pixels. `setFontPx` moves it afterwards. */
  fontPx?: number;
}

export class Screen {
  readonly canvas: HTMLCanvasElement;
  readonly preview: boolean;

  cols = 0;
  rows = 0;
  cursor: [number, number] | null = null;
  cursorShape: CursorShape = "block";
  fontPx: number;
  cellW = 0;
  cellH = 0;
  dpr = 1;

  private readonly opts: ScreenOptions;
  private readonly ctx: CanvasRenderingContext2D;
  /** The old shadow host: what input binds to and what `dropping` lands on. */
  private readonly host: HTMLElement;
  /**
   * Off-screen input sink: keeps a real editable element focused so the browser
   * fires native 'paste' events (whose clipboardData works even in an insecure
   * http context, unlike navigator.clipboard). Null only for a canvas with no
   * parent to hang it off — a <canvas>'s own children are fallback content and
   * are never rendered, so they cannot hold focus.
   */
  private readonly sink: HTMLTextAreaElement | null;
  private readonly _ro: ResizeObserver;
  /**
   * Every listener this class adds — including the one on `window` — is
   * registered with this signal, so `destroy()` removes all of them at once.
   * Without it, re-creating the screen over the same canvas left the old
   * handlers attached: every keystroke was sent twice.
   */
  private readonly _ac: AbortController;
  private readonly _blinkTimer: ReturnType<typeof setInterval>;

  private buf: BufCell[][] = [];
  private _theme: TermTheme;
  private _dirty = false;
  private _raf = 0;
  private _dragging = false;
  private _resizeTimer: ReturnType<typeof setTimeout> | undefined;
  private _blink = true;
  private _under: ImageData | null = null;
  private _underAt: [number, number] = [0, 0];
  /** The old element's `isConnected`: false once `destroy()` has run. */
  private _destroyed = false;
  /**
   * The URLs on the grid as it stands, or null when they have to be found
   * again. Built on demand and only while the pointer is over the canvas —
   * nothing else asks — and dropped by every frame, because a link is a fact
   * about cells and the cells have just changed.
   */
  private _links: ScreenLinks | null = null;
  /** The cell the pointer is over, or null when it is not on the canvas. */
  private _ptr: [number, number] | null = null;
  /** Which link is under it, as an index into the map above. Held only to
   *  notice when it *changes*, which is when the tab's cursor and tooltip have
   *  to move; the underline is resolved from `_ptr` at render time. */
  private _hover: number | null = null;
  /**
   * Whether the program in this pane has asked for the mouse.
   *
   * The rule kitty uses, and for the same reason: over a program that did not
   * ask, a plain click on a link opens it, because there is nothing else the
   * click could sensibly be. Over one that did — `vim`, `htop`, an agent
   * drawing its own UI — the click is the program's and it takes ctrl or cmd
   * to mean the link instead.
   */
  private _wantsMouse = false;

  constructor(canvas: HTMLCanvasElement, opts: ScreenOptions) {
    this.canvas = canvas;
    this.opts = opts;
    const ctx = canvas.getContext("2d", { alpha: false });
    // The old element would have thrown on the first `ctx.font` instead, from
    // somewhere with no useful context. Say it here, where it is true.
    if (!ctx) throw new Error("Screen: this canvas has no 2d context");
    this.ctx = ctx;
    this.host = canvas.parentElement ?? canvas;
    this.preview = opts.preview ?? false;
    this.fontPx = opts.fontPx ?? DEFAULT_FONT_PX;
    this.sink = this.host === canvas ? null : this._makeSink();
    if (!this.preview) canvas.setAttribute("tabindex", "0");

    this._theme = opts.getTheme();
    this._measureCell();

    this._ro = new ResizeObserver(() => this._onResize());
    this._ro.observe(this.host);

    this._ac = new AbortController();
    if (!this.preview) this._bindInput();
    // Cursor blink (visual only; does not touch the protocol).
    this._blinkTimer = setInterval(() => this._blinkTick(), 530);
    // The OS colour scheme, while the page is open.
    //
    // The theme used to be read exactly once, so flipping dark to light left the
    // terminal painted in the palette it was built in until a reload — the cells
    // are drawn to a canvas, and a canvas does not inherit a CSS variable the
    // way the rest of the chrome does. This is the listener that was missing.
    // Registered against the same signal as everything else, so it goes when the
    // screen does.
    if (window.matchMedia) {
      window.matchMedia("(prefers-color-scheme: dark)")
        .addEventListener("change", () => this.refreshTheme(), { signal: this._ac.signal });
    }

    this._onResize();          // establishes cols/rows and emits the first resize
    this._remeasureWhenFontsReady();
    if (!this.preview) queueMicrotask(() => this.focus());
  }

  /// Re-read the palette and repaint.
  ///
  /// Public because the OS is not the only thing that moves it: choosing a
  /// theme on the SETTINGS page rewrites the same variables, and a screen that
  /// only listened to `prefers-color-scheme` would repaint for the OS and not
  /// for the user.
  refreshTheme(): void {
    if (this._destroyed) return;
    this._theme = this.opts.getTheme();
    this._markDirty();
  }

  /** The old `disconnectedCallback`. After this the screen draws nothing. */
  destroy(): void {
    this._destroyed = true;
    this._ro.disconnect();
    this._ac.abort();
    clearInterval(this._blinkTimer);
    clearTimeout(this._resizeTimer);
    cancelAnimationFrame(this._raf);
    // The frame just cancelled is the one that would have cleared this. Leaving
    // it set means every later _markDirty() returns early, so a screen rebuilt
    // over the same canvas keeps applying frames into `buf` and never paints
    // one: a live pane that goes and stays blank.
    this._dirty = false;
    // Ours, appended to somebody else's element — so ours to take away. The
    // element version had the sink inside its own shadow root and got this for
    // free.
    this.sink?.remove();
  }

  private _makeSink(): HTMLTextAreaElement {
    const sink = document.createElement("textarea");
    for (const [k, v] of [["autocapitalize", "off"], ["autocorrect", "off"], ["autocomplete", "off"], ["spellcheck", "false"]]) {
      sink.setAttribute(k!, v!);
    }
    // The shadow stylesheet's rule, inline: there is no stylesheet of our own to
    // put it in any more, and a sink that is visible is a sink that scrolls the
    // page when it is focused.
    sink.style.cssText = "position:absolute;left:0;top:0;width:1px;height:1px;" +
      "opacity:0;border:0;padding:0;margin:0;resize:none;overflow:hidden;" +
      "white-space:nowrap;z-index:-1;";
    this.host.appendChild(sink);
    return sink;
  }

  private _fontStack(): string {
    return `${this.fontPx}px ui-monospace, "SF Mono", "JetBrains Mono", Menlo, Consolas, monospace`;
  }

  private _measureCell(): void {
    this.dpr = window.devicePixelRatio || 1;
    // A throwaway context, so measuring cannot disturb the one we paint with.
    // Falling back to our own is only for a browser that refuses the second
    // context: `font` is set per cell in pass 2 regardless, so it costs nothing.
    const c = document.createElement("canvas").getContext("2d") ?? this.ctx;
    c.font = this._fontStack();
    // Monospace advance width. "M"/"W" are full advance in a monospace font.
    const w = c.measureText("MMMMMMMMMM").width / 10;
    this.cellW = Math.max(1, Math.round(w * 100) / 100);
    this.cellH = Math.round(this.fontPx * 1.34);   // comfortable line box
  }

  // A measurement taken before the font stack has loaded comes from whatever
  // face the browser had to hand, and the cols/rows it implies ride out in the
  // first `hello`. Measure again once the fonts settle and re-grid if the
  // advance width moved; the daemon repaints on the resize that follows.
  private _remeasureWhenFontsReady(): void {
    if (!document.fonts?.ready) return;
    document.fonts.ready.then(() => {
      if (this._destroyed) return;
      const w = this.cellW, h = this.cellH;
      this._measureCell();
      if (this.cellW !== w || this.cellH !== h) this._onResize();
    }).catch(() => {});
  }

  // ---- sizing --------------------------------------------------------------
  private _onResize(): void {
    const rect = this.canvas.getBoundingClientRect();
    if (rect.width < 2 || rect.height < 2) return;
    const cols = Math.max(20, Math.floor(rect.width / this.cellW));
    const rows = Math.max(6, Math.floor(rect.height / this.cellH));

    // Size the backing store to device pixels for crisp text.
    this.dpr = window.devicePixelRatio || 1;
    this.canvas.width = Math.floor(rect.width * this.dpr);
    this.canvas.height = Math.floor(rect.height * this.dpr);
    this.ctx.setTransform(this.dpr, 0, 0, this.dpr, 0, 0);

    if (cols !== this.cols || rows !== this.rows) {
      this.resize(cols, rows);
      clearTimeout(this._resizeTimer);
      this._resizeTimer = setTimeout(() => this._emit(resizeMsg(this.cols, this.rows)), 60);
    }
    this._markDirty();
  }

  /**
   * Re-grid to an explicit size, blanking the buffer.
   *
   * This is the old `_resize`, and it deliberately does *not* tell the daemon:
   * only the `_onResize` path does that, debounced, because that is the one
   * where the size actually changed under us. A caller driving the grid
   * directly (the differential test) has no daemon to tell.
   */
  resize(cols: number, rows: number): void {
    this.cols = cols;
    this.rows = rows;
    this.buf = Array.from({ length: rows }, () => this._blankRow(cols));
    this.cursor = null;
    this._links = null;
  }

  private _blankRow(cols: number): BufCell[] {
    const row: BufCell[] = new Array<BufCell>(cols);
    for (let x = 0; x < cols; x++) row[x] = { ch: " ", fg: "default", bg: "default", mods: null };
    return row;
  }

  setFontPx(px: number): void {
    this.fontPx = Math.max(8, Math.min(40, px));
    this._measureCell();
    this._onResize();
  }

  // ---- frame application ---------------------------------------------------
  applyFrame(frame: FrameUpdate): void {
    if (frame.full) {
      for (let y = 0; y < this.rows; y++) this.buf[y] = this._blankRow(this.cols);
    }
    for (const run of frame.cells || []) {
      let x = run.x;
      const y = run.y;
      if (y >= this.rows) continue;
      // The guard above is the real bounds check; this one is what lets
      // `noUncheckedIndexedAccess` see it.
      const row = this.buf[y];
      if (!row) continue;
      for (const cell of run.cells) {
        if (x < this.cols) {
          // ch === "" is the trailing half of a wide glyph — leave it blank;
          // the wide glyph in the previous cell is drawn spanning two columns.
          row[x] = {
            ch: cell.ch === "" ? "" : (cell.ch || " "),
            fg: cell.fg || "default",
            bg: cell.bg || "default",
            mods: cell.mods || null,
          };
        }
        x++;
      }
    }
    this.cursor = frame.cursor ? [frame.cursor[0], frame.cursor[1]] : null;
    this.cursorShape = frame.cursor_shape || "block";
    this._blink = true;        // show the cursor immediately on activity
    this._links = null;        // the cells moved, so the links did
    this._wantsMouse = !!frame.wants_mouse;
    this._markDirty();
  }

  clear(): void {
    for (let y = 0; y < this.rows; y++) this.buf[y] = this._blankRow(this.cols);
    this.cursor = null;
    this._links = null;
    this._markDirty();
  }

  // ---- rendering -----------------------------------------------------------
  private _markDirty(): void {
    if (this._dirty) return;
    this._dirty = true;
    this._raf = requestAnimationFrame(() => { this._dirty = false; this._render(); });
  }

  // Every `!` below is one fact stated twice: `buf` holds exactly `rows` rows of
  // exactly `cols` cells — `resize()` builds them together and `applyFrame`
  // never writes past either — so no index in these loops can miss. A `?? blank`
  // would invent a cell for a coordinate that cannot happen and hide the day it
  // does.
  private _render(): void {
    const { ctx, cellW, cellH, cols, rows } = this;
    const theme = this._theme;
    if (!cols || !rows) return;
    this._under = null;        // whatever was stashed under the cursor is gone

    // Background wash.
    ctx.fillStyle = theme.bg;
    ctx.fillRect(0, 0, this.canvas.width / this.dpr, this.canvas.height / this.dpr);

    ctx.textBaseline = "alphabetic";
    const baseline = Math.round(cellH * 0.76);
    // Resolved here rather than kept on the pointer handler: a repaint can move
    // the text out from under a stationary pointer, and an underline drawn from
    // a remembered answer would then be under the wrong cells.
    const hover = this._hovered();
    const hoverRuns = (y: number) =>
      hover ? (this._links?.rowRuns(y) ?? []).filter((r) => r.link === hover.link) : [];

    // Pass 1: backgrounds (skip default to keep the wash showing through).
    for (let y = 0; y < rows; y++) {
      const row = this.buf[y]!;
      for (let x = 0; x < cols; x++) {
        const cell = row[x]!;
        const rev = cell.mods && cell.mods.reverse;
        const bg = rev
          ? resolveColor(cell.fg, true, theme)
          : (cell.bg === "default" ? null : resolveColor(cell.bg, false, theme));
        if (bg) {
          ctx.fillStyle = bg;
          ctx.fillRect(x * cellW, y * cellH, cellW + 0.5, cellH);
        }
      }
    }

    // Pass 2: glyphs.
    const plain = this._fontStack();
    for (let y = 0; y < rows; y++) {
      const row = this.buf[y]!;
      for (let x = 0; x < cols; x++) {
        const cell = row[x]!;
        if (cell.ch === "" || cell.ch === " ") {
          if (!(cell.mods && cell.mods.underline)) continue;
        }
        const m = cell.mods;
        const rev = m && m.reverse;
        const fg = rev
          ? resolveColor(cell.bg, false, theme)
          : resolveColor(cell.fg, true, theme);

        ctx.globalAlpha = m && m.dim ? 0.6 : 1;
        let font = plain;
        if (m && (m.bold || m.italic)) {
          font = `${m.italic ? "italic " : ""}${m.bold ? "700 " : ""}${plain}`;
        }
        ctx.font = font;
        ctx.fillStyle = fg;
        if (cell.ch && cell.ch !== " ") {
          ctx.fillText(cell.ch, x * cellW, y * cellH + baseline);
        }
        if (m && m.underline) {
          ctx.fillRect(x * cellW, y * cellH + cellH - 2, cellW, 1);
        }
        if (m && m.crossed_out) {
          ctx.fillRect(x * cellW, y * cellH + Math.round(cellH / 2), cellW, 1);
        }
      }
      // The hovered link, underlined across every row it covers — including the
      // rows of a wrapped one, which is what makes it read as a single address
      // rather than as two lines that happen to be under the pointer.
      for (const run of hoverRuns(y)) {
        ctx.fillStyle = resolveColor(row[run.x0]?.fg ?? "default", true, theme);
        ctx.fillRect(run.x0 * cellW, y * cellH + cellH - 2, (run.x1 - run.x0) * cellW, 1);
      }
    }
    ctx.globalAlpha = 1;
    // The pointer and the tooltip, moved only when the answer changes: both are
    // DOM writes, and this runs on every animation frame.
    if ((hover?.link ?? null) !== this._hover) {
      this._hover = hover?.link ?? null;
      this.canvas.style.cursor = hover ? "pointer" : "";
      if (hover) this.canvas.title = hover.url;
      else this.canvas.removeAttribute("title");
    }

    this._drawCursor();
  }

  // ---- cursor --------------------------------------------------------------
  // **The one structural change in this port.** Keyboard focus lives on the
  // off-screen sink, and the element version sat several shadow roots deep
  // (butai-app → butai-stage → here), so `document.activeElement` was the
  // outermost host and never it; it had to ask the root that actually held the
  // focused node. There are no shadow roots any more, so the plain question is
  // the right one.
  private _hasFocus(): boolean {
    return document.activeElement === (this.sink ?? this.canvas);
  }

  // Draw the cursor, stashing the pixels it covers first so `_hideCursor` can
  // put them back. A blink fires twice a second forever; repainting the whole
  // grid for it (which is what the old `_markDirty` blink did) is thousands of
  // fillText calls a second on an idle pane.
  private _drawCursor(): void {
    if (!this.cursor) return;
    const { ctx, cellW, cellH } = this;
    const theme = this._theme;
    const [cx, cy] = this.cursor;
    if (cx >= this.cols || cy >= this.rows) return;
    const focused = this._hasFocus();
    if (focused && !this._blink) return;         // blinked off
    this._stashUnderCursor(cx, cy);
    if (!focused) {
      // Hollow cursor when unfocused — a steady marker, so it doesn't blink.
      ctx.strokeStyle = theme.fg;
      ctx.lineWidth = 1;
      ctx.strokeRect(cx * cellW + 0.5, cy * cellH + 0.5, cellW - 1, cellH - 1);
      return;
    }
    ctx.fillStyle = theme.fg;
    const x = cx * cellW, y = cy * cellH;
    if (this.cursorShape === "bar") ctx.fillRect(x, y, 2, cellH);
    else if (this.cursorShape === "underline") ctx.fillRect(x, y + cellH - 2, cellW, 2);
    else {
      // Block: fill then redraw the glyph in the background color.
      ctx.fillRect(x, y, cellW, cellH);
      const cell = this.buf[cy]?.[cx];
      if (cell && cell.ch && cell.ch !== " ") {
        ctx.fillStyle = theme.bg;
        ctx.font = this._fontStack();
        ctx.fillText(cell.ch, x, y + Math.round(cellH * 0.76));
      }
    }
  }

  private _hideCursor(): void {
    if (!this._under) return;
    this.ctx.putImageData(this._under, this._underAt[0], this._underAt[1]);
    this._under = null;
  }

  // getImageData/putImageData work in backing-store pixels and ignore the
  // canvas transform, so the rect is scaled by the DPR here and nowhere else.
  private _stashUnderCursor(cx: number, cy: number): void {
    this._under = null;
    const d = this.dpr;
    const x = Math.max(0, Math.floor(cx * this.cellW * d) - 1);
    const y = Math.max(0, Math.floor(cy * this.cellH * d) - 1);
    const w = Math.min(this.canvas.width - x, Math.ceil(this.cellW * d) + 3);
    const h = Math.min(this.canvas.height - y, Math.ceil(this.cellH * d) + 3);
    if (w <= 0 || h <= 0) return;
    try {
      this._under = this.ctx.getImageData(x, y, w, h);
      this._underAt = [x, y];
    } catch {
      this._under = null;     // never let a blink throw
    }
  }

  private _blinkTick(): void {
    this._blink = !this._blink;
    // Nothing to blink, a repaint already queued, or an unfocused (steady)
    // hollow cursor: leave the canvas alone.
    if (!this.cursor || this._dirty || !this._hasFocus()) return;
    if (this._blink) this._drawCursor();
    else this._hideCursor();
  }

  // ---- input ---------------------------------------------------------------
  private _cellAt(ev: MouseEvent): [number, number] {
    const rect = this.canvas.getBoundingClientRect();
    const x = Math.max(0, Math.min(this.cols - 1, Math.floor((ev.clientX - rect.left) / this.cellW)));
    const y = Math.max(0, Math.min(this.rows - 1, Math.floor((ev.clientY - rect.top) / this.cellH)));
    return [x, y];
  }

  // ---- links ---------------------------------------------------------------
  // A URL in a pane is characters and nothing else — the daemon ships cells,
  // and which of them read as an address is the drawing client's question. The
  // terminal client answers it by handing the run to *its* terminal as an OSC 8
  // hyperlink; a canvas has nothing to hand off to, so this hit-tests the same
  // map and opens the tab itself. See `web/src/logic/links.ts`.

  /** The link under a cell, finding the whole grid's links first if needed. */
  private _linkAt(x: number, y: number): { link: number; url: string } | null {
    if (!this._links) {
      this._links = ScreenLinks.of(this.buf.map((row) => row.map((c) => c.ch)));
    }
    return this._links.at(x, y);
  }

  /** Which link the pointer is over right now, or null. */
  private _hovered(): { link: number; url: string } | null {
    if (!this._ptr || this._dragging) return null;
    return this._linkAt(this._ptr[0], this._ptr[1]);
  }

  private _emit(msg: ClientMsg | null): void {
    if (msg) this.opts.onMessage(msg);
  }

  // Report something the user asked for that couldn't be done. The stage draws
  // it; a screen constructed without an `onNotice` just doesn't say.
  private _warn(text: string): void {
    this.opts.onNotice?.(text);
  }

  // Hand one file to the daemon, which writes it beside the workspace and
  // pastes the path into the pane. Images are the reason this exists — agent
  // CLIs read an image from a path — but nothing here is image-specific.
  // Returns true if the file went out. `quiet` suppresses the local flash for a
  // caller that reports the failure itself — the daemon-initiated clipboard
  // read answers over the wire instead, and saying it twice is worse than once.
  private async _sendFile(file: File, quiet = false): Promise<boolean> {
    const warn = (m: string): boolean => { if (!quiet) this._warn(m); return false; };
    if (file.size > MAX_PUT_FILE_BYTES) {
      return warn(`${file.name || "file"} is too large (limit ${MAX_PUT_FILE_BYTES >> 20} MB)`);
    }
    if (!file.size) return false;
    try {
      const bytes = new Uint8Array(await file.arrayBuffer());
      // Clipboard images arrive as "image.png" or with no name at all; the
      // daemon only keeps the extension, so the type is the better source.
      const name = file.name || `paste.${(file.type.split("/")[1] || "bin").replace(/[^a-z0-9]/gi, "")}`;
      this._emit(putFileMsg(name, bytes));
      return true;
    } catch {
      return warn(`could not read ${file.name || "that file"}`);
    }
  }

  // Answer the daemon's `read_clipboard_image`: look for an image on the system
  // clipboard and hand it over as `put_file`. Resolves null when one was sent,
  // or a short reason when there was nothing to send — the caller reports that
  // as a `notice`, because a request that quietly does nothing is
  // indistinguishable from a broken one.
  //
  // This is not the Ctrl-V path. That one runs inside a user gesture and is
  // handled locally; this one arrives unprompted, which is precisely when a
  // browser refuses to read the clipboard — so "the browser said no" is the
  // common answer here, and it is worth saying out loud.
  async readClipboardImage(): Promise<string | null> {
    const read = navigator.clipboard?.read?.bind(navigator.clipboard);
    if (!read) {
      return window.isSecureContext
        ? "this browser cannot read the clipboard"
        : "the browser only reads the clipboard over https (or on localhost)";
    }
    let items: ClipboardItems;
    try {
      items = await read();
    } catch (e) {
      // `e` is `unknown` here, where the JS just reached for `.name`. A
      // DOMException is an Error, which is the case that carries a name worth
      // printing ("NotAllowedError").
      const name = e instanceof Error ? e.name : "";
      return `the browser refused to read the clipboard${name ? ` (${name})` : ""}`;
    }
    for (const item of items || []) {
      const type = (item.types || []).find((t) => t.startsWith("image/"));
      if (!type) continue;
      const blob = await item.getType(type);
      const file = new File([blob], `paste.${type.split("/")[1]}`, { type });
      return (await this._sendFile(file, true)) ? null : "the image on the clipboard could not be read";
    }
    return "no image on the clipboard";
  }

  // A `navigator.clipboard.read()` result: prefer an image arm over the text
  // one, for the reason the `paste` handler prefers files.
  private async _pasteClipboardItems(items: ClipboardItems): Promise<void> {
    for (const item of items || []) {
      const type = item.types.find((t) => t.startsWith("image/"));
      if (type) {
        const blob = await item.getType(type);
        await this._sendFile(new File([blob], `paste.${type.split("/")[1]}`, { type }));
        return;
      }
    }
    for (const item of items || []) {
      if (item.types.includes("text/plain")) {
        const text = await (await item.getType("text/plain")).text();
        if (text) this._emit(pasteMsg(text));
        return;
      }
    }
  }

  // Keep the off-screen <textarea> focused so the browser routes keystrokes,
  // native `paste` events, and IME input to a real editable element.
  focus(): void {
    if (this.sink) this.sink.focus({ preventScroll: true });
    else this.canvas.focus({ preventScroll: true });
  }

  // Take the keyboard off the pane without closing anything. The element
  // version had callers reach through it for `screen.sink.blur()`; the sink is
  // private now, so this is the door.
  blur(): void {
    if (this.sink) this.sink.blur();
    else this.canvas.blur();
  }

  private _bindInput(): void {
    // Every listener below carries this signal so `destroy()` can drop the lot —
    // the `window` one in particular, which nothing else removes.
    const signal = this._ac.signal;
    const host = this.host;
    const on = <K extends keyof HTMLElementEventMap>(
      target: HTMLElement,
      type: K,
      fn: (e: HTMLElementEventMap[K]) => void,
      opts?: AddEventListenerOptions,
    ) => target.addEventListener(type, fn, { ...(opts ?? {}), signal });

    // Keys and paste fire on the focused sink and bubble to the host, which is
    // why they are listened for there and not on the sink itself: tabbing to the
    // canvas (which is a tab stop) has to keep working too.
    on(host, "keydown", (e) => {
      const k = (e.key || "").toLowerCase();
      // Paste: prefer the async Clipboard API when it exists (secure context),
      // but over plain http `navigator.clipboard` is undefined — there we let
      // the native paste event fire on the focused sink (handled below), whose
      // clipboardData works in an insecure context too.
      if ((e.ctrlKey || e.metaKey) && !e.altKey && k === "v") {
        // `read()` returns blobs, so it is the only one of the two that can see
        // an image. Fall back to `readText` when it is missing or refused —
        // Firefox gates `read()` behind a permission `readText` doesn't need,
        // and losing image paste is better than losing paste.
        const readAll = navigator.clipboard?.read?.bind(navigator.clipboard);
        const readText = navigator.clipboard?.readText?.bind(navigator.clipboard);
        if (readAll) {
          e.preventDefault();
          readAll()
            .then((items) => this._pasteClipboardItems(items))
            .catch(() => readText?.().then((t) => { if (t) this._emit(pasteMsg(t)); }))
            .catch(() => {});
        } else if (readText) {
          e.preventDefault();
          readText().then((text) => { if (text) this._emit(pasteMsg(text)); }).catch(() => {});
        }
        return;
      }
      if (isPassthrough(e)) return;             // let the browser handle it
      const msg = keyMsg(e);
      if (msg) {
        e.preventDefault();
        this._emit(msg);
      }
    });

    on(host, "paste", (e) => {
      // `window.clipboardData` is the pre-standard fallback the original kept.
      // It is not in lib.dom, so it is reached through a narrow cast rather than
      // dropped — a browser old enough to need it is a browser this still works
      // in.
      const cd = e.clipboardData ||
        (window as unknown as { clipboardData?: DataTransfer | null }).clipboardData;
      // Files first: a screenshot on the clipboard also carries a text/plain
      // arm on some platforms (a file name, or the empty string), and pasting
      // that instead of the image is the wrong answer every time.
      const file = [...(cd?.files || [])][0];
      if (file) {
        e.preventDefault();
        void this._sendFile(file);
      } else {
        const text = cd?.getData("text");
        if (text) {
          e.preventDefault();
          this._emit(pasteMsg(text));
        }
      }
      if (this.sink) this.sink.value = "";
    });

    // Drag a file onto the pane: same gesture, same destination. `dragover`
    // has to be cancelled or the browser navigates to the file instead.
    on(host, "dragover", (e) => {
      const dt = e.dataTransfer;
      if (!dt || ![...dt.types].includes("Files")) return;
      e.preventDefault();
      dt.dropEffect = "copy";
      host.classList.add("dropping");
    });
    on(host, "dragleave", () => host.classList.remove("dropping"));
    on(host, "drop", (e) => {
      const files = [...(e.dataTransfer?.files || [])];
      if (!files.length) return;
      e.preventDefault();
      host.classList.remove("dropping");
      this.focus();
      // One at a time, in order: each paste lands where the last one left the
      // cursor, so sending them concurrently would interleave the paths.
      void files.reduce<Promise<unknown>>((chain, f) => chain.then(() => this._sendFile(f)), Promise.resolve());
    });

    // Handled keystrokes are preventDefault'd above and never reach the sink;
    // clear anything that slips through (passthrough keys, IME) so it can't
    // accumulate or scroll the hidden field.
    const sink = this.sink;
    if (sink) on(sink, "input", () => { sink.value = ""; });

    // Left button drives the pane: clicks/drag/wheel reach a mouse-hungry app
    // (e.g. Claude), and over an app that doesn't want the mouse a drag paints
    // a server-side text selection copied to the clipboard on release. Hold
    // Alt (or Shift) to force a text selection even over a mouse-hungry app.
    on(host, "mousedown", (e) => {
      const button = mouseButton(e.button);
      if (button === null) return;   // middle/back/forward have no name on the wire
      const [x, y] = this._cellAt(e);
      if (button === "right") {
        // The press is carried so a client with chrome of its own can hang a
        // context menu off it. A `pane` connection has no chrome, so the daemon
        // drops it rather than starting a selection with it — and the browser's
        // own menu is this client's context menu, which is why the default is
        // deliberately NOT prevented here.
        this._emit(mouseMsg("mouse_down", x, y, e.altKey || e.shiftKey, "right"));
        return;
      }
      e.preventDefault();
      // A link takes the click before the pane does — but only when the click
      // could not have meant anything else. Alt or Shift is already this
      // client's "I want a selection, whatever is under me", and a program that
      // asked for the mouse keeps its own clicks unless ctrl or cmd says
      // otherwise. That is the rule kitty and iTerm2 use, so the gesture is the
      // one already in the fingers.
      const link = this._linkAt(x, y);
      if (link && !e.altKey && !e.shiftKey && (!this._wantsMouse || e.ctrlKey || e.metaKey)) {
        // `noopener` for the usual reason: the new tab must not get a handle
        // on this one, and a pane's output is not a trustworthy source of URLs.
        window.open(link.url, "_blank", "noopener,noreferrer");
        return;
      }
      this.focus();
      this._dragging = true;
      this._emit(mouseMsg("mouse_down", x, y, e.altKey || e.shiftKey, "left"));
    });
    on(host, "mousemove", (e) => {
      const [x, y] = this._cellAt(e);
      // Repaint only when the pointer changes cell — a mousemove fires far
      // faster than a frame, and every one of these would otherwise queue one.
      if (!this._ptr || this._ptr[0] !== x || this._ptr[1] !== y) {
        this._ptr = [x, y];
        this._markDirty();
      }
      if (!this._dragging) return;
      this._emit(mouseMsg("mouse_drag", x, y, e.altKey || e.shiftKey));
    });
    // Off the canvas, nothing is hovered — and nothing is scanned either, which
    // is what keeps the map from being rebuilt for a pointer somewhere else on
    // the page.
    on(host, "mouseleave", () => {
      if (!this._ptr) return;
      this._ptr = null;
      this._markDirty();
    });
    // The one listener not on our own subtree: a drag that ends outside the pane
    // still ends the drag. Same signal, so `destroy()` takes it with the rest.
    window.addEventListener("mouseup", (e) => {
      if (!this._dragging) return;
      this._dragging = false;
      const [x, y] = this._cellAt(e);
      this._emit(mouseMsg("mouse_up", x, y));
    }, { signal });

    on(host, "wheel", (e) => {
      e.preventDefault();
      const [x, y] = this._cellAt(e);
      this._emit(scrollMsg(e.deltaY < 0, x, y));
    }, { passive: false });

    on(host, "focusin", () => this._markDirty());
    on(host, "focusout", () => { this._dragging = false; this._markDirty(); });
  }
}
