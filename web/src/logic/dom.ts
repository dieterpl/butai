// Tiny DOM + formatting helpers shared by the chrome components.

import { click, fits, keyText, type Props, type Verb } from "./verbs.ts";
import type { AgentState } from "../protocol/generated/protocol.ts";

/// One child `h()` accepts, or a list of them — `kids.flat()` is what makes
/// `h("div", {}, rows.map(...))` read the same as spelling the rows out.
type Child = Node | string | number | boolean | null | undefined;

// h('div', {class:'x', onclick:fn}, child, child...) -> HTMLElement
export function h(tag: string, attrs?: Props | null, ...kids: (Child | readonly Child[])[]): HTMLElement {
  const e = document.createElement(tag);
  if (attrs) {
    // `verbs.ts` calls [`Props`] "the props `h()` wants", and its values are
    // `unknown` on purpose: this is the renderer, so this is where an attribute
    // stops being an unknown and becomes a class, a listener, a style object or
    // a string the DOM will coerce.
    for (const [k, v] of Object.entries(attrs)) {
      if (v == null || v === false) continue;
      if (k === "class") e.className = String(v);
      else if (k === "html") e.innerHTML = String(v);
      else if (k === "style" && typeof v === "object") Object.assign(e.style, v);
      // Narrowed to `Function`, which is as far as `typeof` goes; the DOM wants
      // the shape, and every listener that reaches here came from `click()`.
      else if (k.startsWith("on") && typeof v === "function") e.addEventListener(k.slice(2), v as EventListener);
      else e.setAttribute(k, v === true ? "" : String(v));
    }
  }
  for (const kid of kids.flat()) {
    if (kid == null || kid === false) continue;
    // `typeof kid === "object"` is only there so the compiler can follow the
    // `kid.nodeType` test JavaScript could make on its own.
    e.append(typeof kid === "object" && kid.nodeType ? kid : document.createTextNode(String(kid)));
  }
  return e;
}

export function clear<T extends Node>(node: T): T {
  while (node.firstChild) node.removeChild(node.firstChild);
  return node;
}

/// An agent row's `[glyph, cls, label]`.
type Mark = readonly [glyph: string, cls: string, label: string];

// Attention mark + class for an agent state. One arm per AgentState variant the
// daemon actually has (crates/butai-protocol/src/api.rs) — no more, no less,
// which `satisfies Record<AgentState, …>` now holds up rather than the comment.
export const AGENT_MARK = {
  waiting: ["[?]", "needs", "waiting on you"],
  working: ["[~]", "work", "working"],
  finished: ["[v]", "done", "done — your turn"],
  idle: ["[ ]", "idle", "idle"],
  exited: ["[x]", "dead", "exited"],
} as const satisfies Record<AgentState, Mark>;

// The mark on a status word whose turn has not been read — the web spelling of
// `chrome::model::UNREAD_MARK`. Appended, never substituted, so the word keeps
// the spelling it always had.
export const UNREAD_MARK = "•";

/// Decorate an agent row's `[glyph, cls, label]` with whether it is unread.
///
/// Unread keeps the state's own class and takes the mark; read drops to `idle`,
/// which is the CSS that greys a row out. Only `finished` and `exited` can be
/// unread — `waiting` is urgent however often it has been read, so it is
/// returned untouched and never fades.
export function markUnread([glyph, cls, label]: Mark, unread: boolean): Mark {
  if (unread) return [glyph, cls, label + " " + UNREAD_MARK];
  if (cls === "done") return [glyph, "idle", label];
  return [glyph, cls, label];
}

// Process status -> css class (mirrors the TUI colors).
export function procClass(status: string) {
  if (status === "ok") return "ok";
  if (status === "done") return "done";
  if (status === "...") return "busy";
  if (status && status.startsWith("FAIL")) return "fail";
  return "run";
}

export function fmtGb(n: number) {
  return (Math.round(n * 10) / 10).toFixed(1);
}

// 8-level block sparkline from a 0..100 value list (like the TUI's SYSTEM rail).
const BLOCKS = ["▁", "▂", "▃", "▄", "▅", "▆", "▇", "█"] as const;
export function sparkline(vals: readonly number[], width = 12) {
  const tail = vals.slice(-width);
  const pad = width - tail.length;
  let s = BLOCKS[0].repeat(Math.max(0, pad));
  for (const v of tail) {
    const i = Math.max(0, Math.min(7, Math.round((v / 100) * 7)));
    // Clamped on the line above to exactly the range `BLOCKS` has, which is
    // the fact the compiler cannot see and the `!` states.
    s += BLOCKS[i]!;
  }
  return s;
}

export function loadClass(pct: number) {
  return pct >= 85 ? "bad" : pct >= 50 ? "warn" : "ok";
}

// A verb footer, packed exactly as the terminal packs it.
//
// The browser's boxes are elastic and could fit more, but the packing is not a
// layout decision — it is *which verbs are worth a column*, and the answer has
// to be the same one the TUI gives or the two clients teach different keys. The
// widths come from `crates/butai-client/src/chrome/`: LEFT_W - 2, RIGHT_W - 2,
// and `git_list_width`'s upper clamp - 2.
//
// A footer word *is* a key: the click resolves to the letter that was drawn and
// goes through the same dispatch the keystroke does, which is what stops the
// two from ever disagreeing. Lives here rather than in butai-app.js because the
// GIT page draws two of them and importing the shell from a page it contains is
// a cycle waiting to be a load-order bug.
export const RAIL_COLS = 26;
export const CHANGES_COLS = 36;
export const GIT_COLS = 50;

/// The shell, reached the way a footer word reaches it: through the window.
/// `butai-app.js` owns the global and is still JavaScript, so the shape is
/// spelled out here rather than declared on `Window` — a global augmentation
/// from this file would be this file typing somebody else's property.
type ButaiWindow = { readonly __butaiApp: { pressVerb(surface: string, key: string): void } };

export function drawFooter(
  el: HTMLElement | null | undefined,
  verbs: readonly Verb[],
  surface: string,
  cols = RAIL_COLS,
  rows = 1,
) {
  if (!el) return;
  clear(el);
  for (const v of fits(verbs, cols, rows)) {
    el.append(h("button",
      click("rail.verb", () => (window as unknown as ButaiWindow).__butaiApp.pressVerb(surface, v.key),
        { class: "fv" + (v.danger ? " danger" : ""), title: v.label }),
      h("b", {}, keyText(v.key)), " " + v.label));
  }
}
