// A window that says something, may ask for a line of text, and can say why the
// answer did not work.
//
//     ┌ ▲ UNKNOWN HOST ──────────────────────────────────┐
//     │ The authenticity of macmini can't be established. │
//     │ type    ED25519                                   │
//     │ sha256  hnZRMnSrfeIwqOHlgb106J8zCBjGGDvZgQhqkuMz6NI│
//     │ > ••••••                                          │
//     │ ✕ Permission denied, please try again.            │
//     ├───────────────────────────────────────────────────┤
//     │                          [ cancel ] [ trust ]     │
//     └───────────────────────────────────────────────────┘
//
// The terminal's `chrome/dialog.rs` is the same component: a tone, a lead, a
// block of facts, a note, an input, an error, and buttons. The shell's two older
// modals — a yes/no and a form — are this with fewer parts, so they are drawn by
// it, and every question the shell asks can now say it failed without closing.
//
// **An action that fails keeps the window open.** `run` may return a message,
// and the window shows it under the fields and stays up, with what was typed
// still in the boxes. That is the whole reason this exists as one component: a
// toast about a refused password, over a window that has already gone, leaves
// someone retyping a thing they cannot see.

import * as React from "react";

import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Notice } from "@/components/Notice";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { cn } from "@/lib/utils";

/** How alarming it is. Picks the frame and the title mark, nothing else. */
export type WindowTone = "info" | "warn" | "danger";

export interface WindowFact {
  label: string;
  value: string;
  /** Colour of the value; the ordinary pen when absent. */
  tone?: "ok" | "warn" | "bad" | undefined;
}

export interface WindowField {
  label: string;
  value?: string | undefined;
  /** A choice rather than free text. */
  options?: readonly string[] | undefined;
  /** Drawn as dots. A passphrase on a screen someone may be recording is published. */
  secret?: boolean | undefined;
}

export interface WindowAction {
  label: string;
  /** What `Enter` presses, when a field has the keyboard. */
  primary?: boolean | undefined;
  destructive?: boolean | undefined;
  /** Focused when the window opens. Give this to the safe answer, never to a trust. */
  autoFocus?: boolean | undefined;
  /**
   * Do it. Return a message to keep the window open and say why it did not
   * work; return nothing and the window closes.
   */
  run: (values: string[]) => void | string | null | Promise<void | string | null>;
}

export interface WindowProps {
  tone?: WindowTone;
  title: string;
  lead?: readonly string[];
  facts?: readonly WindowFact[];
  note?: readonly string[];
  fields?: readonly WindowField[];
  /** A failure already known when it opens. */
  error?: string | null | undefined;
  actions: readonly WindowAction[];
  /**
   * It is over. `"dismissed"`: Esc, the corner, or a click outside, with no
   * answer. `"done"`: an action ran and did not report a failure.
   */
  onClose: (how: "dismissed" | "done") => void;
}

const MARK: Record<WindowTone, string> = { info: "", warn: "▲ ", danger: "✕ " };
const FRAME: Record<WindowTone, string> = {
  info: "",
  warn: "shadow-[inset_0_0_0_1px_var(--color-warn)]",
  danger: "shadow-[inset_0_0_0_1px_var(--color-bad)]",
};
const HEADER: Record<WindowTone, string> = {
  info: "",
  warn: "text-warn before:border-warn",
  danger: "text-bad before:border-bad",
};
const FACT_TONE = { ok: "text-ok", warn: "text-warn", bad: "text-bad" } as const;

const replace = <T,>(xs: readonly T[], i: number, v: T): T[] => xs.map((x, j) => (j === i ? v : x));

export function Window({
  tone = "info",
  title,
  lead = [],
  facts = [],
  note = [],
  fields = [],
  error: known,
  actions,
  onClose,
}: WindowProps) {
  const [vals, setVals] = React.useState<string[]>(() => fields.map((f) => f.value ?? ""));
  const [error, setError] = React.useState<string | null>(known ?? null);
  const [pending, setPending] = React.useState(false);

  const act = async (a: WindowAction) => {
    if (pending) return;
    setPending(true);
    setError(null);
    let why: void | string | null;
    try {
      why = await a.run(vals);
    } catch (e) {
      why = e instanceof Error ? e.message : String(e);
    }
    setPending(false);
    if (why) setError(why);
    else onClose("done");
  };
  const primary = actions.find((a) => a.primary);

  return (
    <Dialog open onOpenChange={(o) => { if (!o) onClose("dismissed"); }}>
      <DialogContent
        className={FRAME[tone]}
        onKeyDown={(e) => {
          // Enter submits a form. In a window with no fields it is left to the
          // focused button, so a trust is never one stray keystroke.
          if (e.key === "Enter" && !e.shiftKey && fields.length > 0 && primary && !(e.target instanceof HTMLButtonElement)) {
            e.preventDefault();
            void act(primary);
          }
        }}
      >
        <DialogHeader className={HEADER[tone]}>
          <DialogTitle>
            {MARK[tone]}
            {title}
          </DialogTitle>
        </DialogHeader>
        {lead.length > 0 ? (
          <DialogDescription asChild>
            <div className="grid gap-1 text-foreground">
              {lead.map((p, i) => (
                <p key={i}>{p}</p>
              ))}
            </div>
          </DialogDescription>
        ) : null}
        {facts.length > 0 ? (
          <dl className="grid grid-cols-[max-content_minmax(0,1fr)] gap-x-2 text-13">
            {facts.map((f) => (
              <React.Fragment key={f.label}>
                <dt className="text-faint">{f.label}</dt>
                <dd className={cn("m-0 break-all font-mono", f.tone && FACT_TONE[f.tone])}>{f.value}</dd>
              </React.Fragment>
            ))}
          </dl>
        ) : null}
        {note.map((p, i) => (
          <p key={i} className="text-13 text-faint">
            {p}
          </p>
        ))}
        {fields.map((f, i) => (
          <label key={f.label} className="grid gap-1">
            <span className="text-11 text-dim">{f.label}</span>
            {f.options ? (
              <Select value={vals[i] ?? ""} onValueChange={(v) => setVals((cur) => replace(cur, i, v))}>
                <SelectTrigger className="w-full" aria-label={f.label}>
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  {f.options.map((o) => (
                    <SelectItem key={o} value={o}>
                      {o}
                    </SelectItem>
                  ))}
                </SelectContent>
              </Select>
            ) : (
              <Input
                autoFocus={i === 0}
                aria-label={f.label}
                aria-invalid={error ? true : undefined}
                type={f.secret ? "password" : "text"}
                autoComplete={f.secret ? "off" : undefined}
                value={vals[i] ?? ""}
                onChange={(e) => {
                  setVals((cur) => replace(cur, i, e.target.value));
                  setError(null);
                }}
              />
            )}
          </label>
        ))}
        {error ? (
          <Notice variant="bad" role="alert">
            ✕ {error}
          </Notice>
        ) : null}
        <DialogFooter>
          {actions.map((a) => (
            <Button
              key={a.label}
              variant={a.destructive ? "destructive" : a.primary ? "default" : "outline"}
              autoFocus={a.autoFocus}
              disabled={pending}
              onClick={() => void act(a)}
            >
              {a.label}
            </Button>
          ))}
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
