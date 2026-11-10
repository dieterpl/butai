// HELP's Keys topic: the keyboard, with what each key does printed on it.
//
// **Drawn from `logic/keyboard.ts`, which is drawn from `verbs.ts`.** Not one
// key letter or label is written in this file — the same rule as every other
// HELP topic, for the same reason: a reference written by hand beside the tables
// is a second reference, and the two disagree the first time a key moves. What
// is here is the drawing, and the four things you can do with it: pick a layer,
// search, point at a key, or press one.
//
// ## The terminal's look, not a keyboard's
//
// A keycap would naturally be rounded, raised and lit. This kit has no radius,
// no shadow and no motion (`components/KIT.md`), so a cap is a ruled box on the
// terminal's 18px line — three of them: the key, then two lines of what it
// does. The colour on its left edge is the verb's category; the held modifier
// is the accent band, which is what the kit uses for "the thing you are on".
//
// ## Your own keyboard, without taking it from the page
//
// The shell's capture listener sees every key first and keeps the ones it
// binds — `esc` still leaves HELP, `?` still toggles it, an Alt key the
// workbench binds still runs. What reaches this listener is what nothing else
// wanted, and it only *looks* (no `preventDefault`), so the page's own `j`/`k`/
// `home`/`end` still scroll the topic. Holding Alt shows the Alt layer while it
// is held: the shell passes a bare Alt through, because no table binds it.

import * as React from "react";

import { Button } from "@/components/ui/button";
import { Card } from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import { SectionTitle } from "@/components/SectionTitle";
import { cn } from "@/lib/utils";
import {
  CATEGORIES,
  MAIN,
  NAV,
  capById,
  capForCode,
  isGap,
  keyCount,
  layers,
  matches,
  onCap,
  type Binding,
  type Cap,
  type Category,
  type Layer,
  type Slot,
} from "@/logic/keyboard.ts";

/**
 * The pen per category. Tokens only — `KIT.md`'s rule — and the one that is
 * not a role is a `color-mix()` of two, because the palette has five hues and
 * the keyboard needs six.
 */
const PEN: Readonly<Record<Category, string>> = {
  view: "var(--color-primary)",
  agent: "var(--color-ok)",
  git: "var(--color-warn)",
  project: "color-mix(in oklab, var(--color-primary) 50%, var(--color-bad))",
  tool: "var(--color-run)",
  danger: "var(--color-bad)",
  program: "var(--color-faint)",
};

/** The modifiers' caps: pressing one is not a lookup. */
const MODIFIER_CAPS = new Set([
  "shiftleft", "shiftright", "controlleft", "controlright", "altleft", "altright",
  "metaleft", "metaright", "capslock",
]);

const MARK: Readonly<Record<Binding["mod"], string>> = { "": "", shift: "⇧ ", ctrl: "⌃ " };

/** Labels on one cap and modifier, once each: `diff · show`. */
function legend(bs: readonly Binding[]): string {
  return [...new Set(bs.map((b) => b.label))].join(" · ");
}

function isField(t: EventTarget | null): boolean {
  const el = t as HTMLElement | null;
  if (!el?.tagName) return false;
  const tag = el.tagName.toLowerCase();
  return tag === "input" || tag === "textarea" || tag === "select" || el.isContentEditable;
}

export interface KeysTopicProps {
  /** The prefix as the user spells it; the prefix layer is reached by theirs. */
  prefix?: string | undefined;
}

export function KeysTopic({ prefix }: KeysTopicProps) {
  const all = React.useMemo(() => layers(prefix || "C-b"), [prefix]);
  const [layerId, setLayerId] = React.useState(all[0]?.id ?? "alt");
  const [query, setQuery] = React.useState("");
  const [focus, setFocus] = React.useState<string | null>(null);
  const [pressed, setPressed] = React.useState<ReadonlySet<string>>(new Set());
  const search = React.useRef<HTMLInputElement>(null);
  // The layer to go back to when a held Alt is let go; null while none is held.
  const beforeAlt = React.useRef<string | null>(null);

  const layer = all.find((l) => l.id === layerId) ?? all[0]!;
  const hit = React.useCallback((b: Binding) => matches(b, query), [query]);
  const searching = query.trim() !== "";

  // The cap the detail describes: the one pointed at or pressed, else the first
  // this layer binds that the search lets through.
  const shown = focus ?? layer.bindings.find(hit)?.cap ?? null;

  React.useEffect(() => {
    const press = (e: KeyboardEvent) => {
      if (isField(e.target)) return;
      if (e.key === "Alt" && !e.ctrlKey && !e.metaKey) {
        if (beforeAlt.current === null) {
          beforeAlt.current = layerId;
          setLayerId("alt");
        }
      } else if (e.key === "/" && !e.ctrlKey && !e.altKey && !e.metaKey && !e.defaultPrevented) {
        e.preventDefault();
        search.current?.focus();
        return;
      }
      const cap = capForCode(e.code);
      if (!cap) return;
      setPressed((s) => new Set(s).add(cap.id));
      if (!MODIFIER_CAPS.has(cap.id)) setFocus(cap.id);
    };
    const release = (e: KeyboardEvent) => {
      const cap = capForCode(e.code);
      if (cap) setPressed((s) => { const n = new Set(s); n.delete(cap.id); return n; });
      if (e.key === "Alt" && beforeAlt.current !== null) {
        setLayerId(beforeAlt.current);
        beforeAlt.current = null;
      }
    };
    // A key held while the window loses focus never sends its keyup here.
    const blur = () => {
      setPressed(new Set());
      if (beforeAlt.current !== null) {
        setLayerId(beforeAlt.current);
        beforeAlt.current = null;
      }
    };
    window.addEventListener("keydown", press);
    window.addEventListener("keyup", release);
    window.addEventListener("blur", blur);
    return () => {
      window.removeEventListener("keydown", press);
      window.removeEventListener("keyup", release);
      window.removeEventListener("blur", blur);
    };
  }, [layerId]);

  const choose = (id: string) => {
    beforeAlt.current = null;
    setLayerId(id);
  };

  return (
    <div data-slot="help-keys" className="min-w-0 p-3 font-mono text-13 text-foreground">
      <h1 className="mb-2 font-semibold text-primary">Keys</h1>

      <div className="mb-2 flex flex-wrap items-center gap-x-2 gap-y-1">
        <div role="group" aria-label="layer" className="flex flex-wrap gap-x-1 gap-y-1">
          {all.map((l) => {
            const on = l.id === layer.id;
            const n = keyCount(l, searching ? hit : undefined);
            return (
              <Button
                key={l.id}
                size="sm"
                variant={on ? "default" : "ghost"}
                aria-pressed={on}
                onClick={() => choose(l.id)}
                className={cn(!on && "text-dim")}
              >
                <span className={cn(!on && l.chord && "text-primary")}>{l.chord || l.title}</span>
                {l.chord ? <span>layer</span> : null}
                <span className={cn("text-11", on ? "" : searching && n ? "text-primary" : "text-faint")}>{n}</span>
              </Button>
            );
          })}
        </div>
        <label className="ml-auto flex min-w-56 items-center gap-1 text-faint">
          <span aria-hidden="true">/</span>
          <Input
            ref={search}
            value={query}
            placeholder="find a key — git, agent, close…"
            aria-label="find a key"
            className="border-0 border-b px-1"
            onChange={(e) => {
              setQuery(e.target.value);
              setFocus(null);
            }}
            onKeyDown={(e) => {
              if (e.key !== "Escape") return;
              // The search's own esc: clear it and give the keyboard back to the
              // page, so the *next* esc is the one that leaves HELP.
              e.preventDefault();
              e.stopPropagation();
              setQuery("");
              e.currentTarget.closest<HTMLElement>('[data-page="help"]')?.focus({ preventScroll: true });
            }}
          />
          {searching ? <span className="shrink-0 text-11">{keyCount(layer, hit)} here</span> : null}
        </label>
      </div>

      <p className="mb-2 max-w-[78ch] text-dim">
        {layer.id === "alt" ? (
          <><Kbd>alt</Kbd> held, then the key. </>
        ) : layer.id === "prefix" ? (
          <><Kbd>{layer.chord}</Kbd> pressed and let go, then the key. </>
        ) : null}
        {layer.note}
      </p>

      <Board layer={layer} shown={shown} pressed={pressed} hit={searching ? hit : null} onPoint={setFocus} />

      <div className="mt-3 grid grid-cols-1 gap-3 lg:grid-cols-[minmax(18rem,1fr)_2fr]">
        <Card className="min-h-28">
          <SectionTitle>key</SectionTitle>
          <Detail all={all} layer={layer} cap={shown} />
        </Card>
        <Card>
          <SectionTitle>{`${layer.chord ? layer.chord + " layer" : layer.title} · ${keyCount(layer)} keys`}</SectionTitle>
          <List layer={layer} hit={hit} shown={shown} onPoint={setFocus} />
          <Legend layer={layer} />
        </Card>
      </div>
    </div>
  );
}

// ---------------------------------------------------------------------------
// The board
// ---------------------------------------------------------------------------

interface BoardProps {
  layer: Layer;
  shown: string | null;
  pressed: ReadonlySet<string>;
  /** The search, while there is one. */
  hit: ((b: Binding) => boolean) | null;
  onPoint: (cap: string) => void;
}

/**
 * Both blocks, scaled to the width they are given.
 *
 * `--u` is one key unit: the topic's width over the 18 units an ANSI board and
 * its nav cluster span — less the 90px of gaps, padding and frame between and
 * around them — floored where a two-line legend stops being readable at the
 * annotation size and capped where the caps would only get emptier. Narrower
 * than the floor, the board scrolls sideways.
 */
function Board({ layer, shown, pressed, hit, onPoint }: BoardProps) {
  const row = (slots: readonly Slot[], i: number) => (
    <div key={i} className="flex h-(--h) gap-1">
      {slots.map((s, j) =>
        isGap(s) ? (
          <div key={j} aria-hidden="true" style={{ width: `calc(var(--u) * ${s.gap} + 4px * ${Math.max(0, s.gap - 1)})` }} />
        ) : (
          <KeyCap key={s.id} cap={s} layer={layer} shown={shown === s.id} pressed={pressed.has(s.id)} hit={hit} onPoint={onPoint} />
        ),
      )}
    </div>
  );
  return (
    <div className="@container overflow-x-auto pb-1">
      <div
        aria-hidden="true"
        className="flex w-max gap-3 border border-border bg-card p-2"
        style={{
          ["--u" as string]: "clamp(48px, calc((100cqw - 92px) / 18), 88px)",
          ["--h" as string]: "calc(var(--spacing-row) * 3 + 2px)",
        }}
      >
        <div className="flex flex-col gap-1">
          {MAIN.map((r, i) => (i === 0 ? <div key={i} className="mb-2">{row(r, i)}</div> : row(r, i)))}
        </div>
        <div className="flex flex-col gap-1">
          {/* Level with the number row: the F-row, and the gap under it. */}
          <div className="mb-2 h-(--h)" />
          {NAV.map(row)}
        </div>
      </div>
    </div>
  );
}

interface KeyCapProps {
  cap: Cap;
  layer: Layer;
  shown: boolean;
  pressed: boolean;
  hit: ((b: Binding) => boolean) | null;
  onPoint: (cap: string) => void;
}

function KeyCap({ cap, layer, shown, pressed, hit, onPoint }: KeyCapProps) {
  const bs = onCap(layer, cap.id);
  const base = bs.filter((b) => b.mod === "");
  const modded = (["shift", "ctrl"] as const)
    .map((mod) => ({ mod, bs: bs.filter((b) => b.mod === mod) }))
    .filter((x) => x.bs.length);
  const first = base[0] ?? modded[0]?.bs[0];
  const held = layer.held.includes(cap.id);
  const isPrefix = layer.prefixCap === cap.id;
  const program = first?.source === "fallthrough";
  const match = hit != null && bs.some(hit);
  const w = cap.w ?? 1;
  return (
    <div
      data-cap={cap.id}
      data-bound={first ? "" : undefined}
      onMouseEnter={() => onPoint(cap.id)}
      onClick={() => onPoint(cap.id)}
      style={{ width: `calc(var(--u) * ${w} + 4px * ${w - 1})` }}
      className={cn(
        "relative flex h-full shrink-0 cursor-default flex-col justify-between overflow-hidden border px-1 pl-2",
        "border-border bg-background",
        first && "border-dim",
        program && "bg-[repeating-linear-gradient(135deg,var(--color-muted)_0_4px,transparent_4px_8px)]",
        shown && "bg-sel",
        isPrefix && "border-primary",
        hit && first && !match && "opacity-25",
        match && "border-primary",
        pressed && "border-foreground bg-sel",
        held && "border-primary bg-primary text-primary-foreground",
      )}
    >
      {first ? (
        <span aria-hidden="true" className="absolute top-1 bottom-1 left-0 w-[3px]" style={{ background: PEN[first.category] }} />
      ) : null}
      <span className="flex items-baseline justify-between gap-1 leading-[18px]">
        <span className={cn(cap.mod ? "text-11" : "text-13", !held && (first ? "text-foreground" : "text-faint"))}>{cap.glyph}</span>
        {cap.shift ? <span className={cn("text-11", !held && "text-faint")}>{cap.shift}</span> : null}
      </span>
      <span className="flex min-w-0 flex-col text-11 leading-[18px]">
        {modded.map((x) => (
          <span key={x.mod} className="truncate" style={{ color: PEN[x.bs[0]!.category] }}>
            <span className="text-faint">{MARK[x.mod]}</span>
            {legend(x.bs)}
          </span>
        ))}
        {base.length ? (
          <span className={cn("break-words", modded.length ? "line-clamp-1" : "line-clamp-2", program ? "text-faint" : "text-foreground")}>
            {legend(base)}
          </span>
        ) : null}
      </span>
    </div>
  );
}

// ---------------------------------------------------------------------------
// The key, the layer, the legend
// ---------------------------------------------------------------------------

function Kbd({ children }: { children: React.ReactNode }) {
  return <kbd className="border border-dim px-1 font-mono text-foreground">{children}</kbd>;
}

/** `alt` `o`, `C-b` then `A`, or the bare key. */
function Combo({ layer, b }: { layer: Layer; b: Binding }) {
  const press = layer.chord ? b.spelled.slice(layer.chord.length + 1) : b.spelled;
  return (
    <span className="flex items-center gap-1">
      {layer.id === "alt" ? <Kbd>alt</Kbd> : null}
      {layer.id === "prefix" ? <><Kbd>{layer.chord}</Kbd><span className="text-faint">then</span></> : null}
      <Kbd>{press}</Kbd>
    </span>
  );
}

function Detail({ all, layer, cap }: { all: readonly Layer[]; layer: Layer; cap: string | null }) {
  if (!cap) return <p className="px-2 py-1 text-dim">Point at a key, or press one.</p>;
  const here = onCap(layer, cap);
  const glyph = capById(cap)?.glyph ?? cap;
  // What the same key does on the other layers — which is what you get when you
  // are on the wrong one.
  const elsewhere = all
    .filter((l) => l.id !== layer.id)
    .map((l) => ({ l, bs: onCap(l, cap) }))
    .filter((x) => x.bs.length);
  return (
    <div className="flex flex-col gap-2 px-2 py-1">
      {here.length ? (
        here.map((b, i) => (
          <div key={i}>
            <Combo layer={layer} b={b} />
            <div className="mt-1" style={{ color: PEN[b.category] }}>{b.label}</div>
            {b.note ? <div className="text-11 text-dim">{b.note}</div> : null}
            {b.source === "alias" ? <div className="text-11 text-faint">the same key as {b.aliasOf}</div> : null}
            {b.quiet ? <div className="text-11 text-faint">bound, not in the footer</div> : null}
          </div>
        ))
      ) : (
        <div>
          <Kbd>{glyph}</Kbd>
          <div className="mt-1 text-dim">
            {layer.prefixCap === cap
              ? `the prefix — ${layer.chord} twice sends one to the program`
              : "nothing on this layer"}
          </div>
        </div>
      )}
      {elsewhere.length ? (
        <div className="text-11 text-faint">
          elsewhere:{" "}
          {elsewhere.map(({ l, bs }, i) => (
            <span key={l.id}>
              {i ? " · " : ""}
              <span className="text-dim">{l.chord || l.title}</span>{" "}
              {bs.map((b) => MARK[b.mod] + b.label).join(", ")}
            </span>
          ))}
        </div>
      ) : null}
    </div>
  );
}

function List({ layer, hit, shown, onPoint }: {
  layer: Layer;
  hit: (b: Binding) => boolean;
  shown: string | null;
  onPoint: (cap: string) => void;
}) {
  const order = CATEGORIES.map((c) => c.id);
  const rows = [...layer.bindings].sort((a, b) => order.indexOf(a.category) - order.indexOf(b.category));
  return (
    <div className="columns-1 gap-4 px-2 py-1 sm:columns-2">
      {rows.map((b, i) => (
        <div
          key={i}
          onMouseEnter={() => onPoint(b.cap)}
          className={cn(
            "flex break-inside-avoid gap-2 border-l-[3px] pl-1",
            b.cap === shown && "bg-sel",
            !hit(b) && "opacity-30",
          )}
          style={{ borderLeftColor: PEN[b.category] }}
        >
          <span className="w-28 shrink-0 truncate text-primary">{b.spelled}</span>
          <span className="min-w-0 truncate">{b.label}</span>
        </div>
      ))}
    </div>
  );
}

function Legend({ layer }: { layer: Layer }) {
  const used = new Set(layer.bindings.map((b) => b.category));
  return (
    <div className="mx-2 mt-1 mb-2 flex flex-wrap gap-x-3 border-t border-dashed border-border pt-1 text-11 text-dim">
      {CATEGORIES.filter((c) => used.has(c.id)).map((c) => (
        <span key={c.id} className="flex items-center gap-1">
          <span aria-hidden="true" className="inline-block size-2" style={{ background: PEN[c.id] }} />
          {c.label}
        </span>
      ))}
    </div>
  );
}

export default KeysTopic;
