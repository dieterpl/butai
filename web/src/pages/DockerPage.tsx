// DOCKER — compose stacks on the left, one of their logs on the right.
//
// The port of `web/ui/page-docker.js`, itself the browser's version of the
// terminal's docker space. The page's whole argument survives unchanged:
//
//   > Following is not a special channel. The client spawns `docker logs -f` as
//   > an ordinary **process pane** and streams it, which is exactly what the
//   > Mac client does and is the reason the daemon needs no docker-log route.
//
// So this file draws a list and a bar, and everything live on it is a pane the
// daemon is already streaming through `<Stage>` — the boundary rule is that the
// daemon renders a pane's screen and the client draws everything else. Every
// action is likewise a *command in a pane* rather than a route: the daemon runs
// docker the way you would, which is what keeps it out of the business of
// knowing docker.
//
// ## The cursor and the acting row are two things
//
//   > `enter` is what points the log pane at the row under the cursor; the
//   > other verbs act on the row the *bar* is showing. It is the one surface
//   > here where the cursor and the acting row can differ.
//
// Restarting whatever your cursor happens to be resting on, while the logs on
// screen are a different container's, is the accident that shape prevents.
//
// Two rows in one list therefore have to be told apart, and this is exactly
// where the vanilla page grew a second selection style — a band for the followed
// row, an outline for the cursor. They are not the same kind of thing, so they
// do not both want a `Row` state: **the followed row is `selected`, and the
// cursor is focus.** `Row` already draws a keyboard focus ring and already takes
// `Enter`, so moving the cursor moves the browser's own focus, which also
// scrolls the row into view and tells a screen reader where it is. One band, one
// ring, and neither of them invented here.
//
// The one thing that is not the vanilla's: with nothing followed yet, the action
// verbs act on the cursor's row rather than doing nothing at all. The bar is
// what they aim at the moment there *is* one — the rule that matters is "do not
// act on a row you are not looking at", and before anything is on screen there
// is no other row to confuse it with. A footer that advertises `r restart` and
// answers with silence is the failure this page is being rewritten to stop
// having.

import { useCallback, useEffect, useMemo, useRef, useState } from "react";

import { Empty } from "@/components/Empty";
import { HintBar, type Hint } from "@/components/HintBar";
import { Row } from "@/components/Row";
import { SectionTitle } from "@/components/SectionTitle";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { ScrollArea } from "@/components/ui/scroll-area";

import type { World } from "@/app/world.ts";
import type { Qid, QualifiedWorkspace } from "@/logic/events.ts";
import type { TermTheme } from "@/logic/palette.ts";
import { RAIL_COLS } from "@/logic/dom.ts";
import { DockerRow, MAX_ROWS, VerbId, click, dockerVerbs, keyName, type TargetId } from "@/logic/verbs.ts";
import { hints } from "@/pages/parts.ts";
import type { ContainerDto, StackDto } from "@/protocol/generated/protocol.ts";
import { Stage, type StageEvents } from "@/stage/Stage";

/** A process pane to open: what to call it, and what to run in it. */
export interface DockerRun {
  name: string;
  command: string;
}

/**
 * What the page asks the shell to do. Both are one thing — run a command in a
 * process pane — and neither is `spawn`, which on this client means an *agent*.
 */
export interface DockerActions {
  /**
   * Follow this stack or container: open the process pane named `name`, and reap
   * whichever follower was running before it.
   *
   * The reaping is the shell's rather than the page's because it is a *write*,
   * and because it has to keep happening when the page is gone — a
   * `docker logs -f` follower is a live process with a PTY behind it, and one
   * leaked per click is a machine full of them by lunchtime.
   */
  logs(req: DockerRun): void;
  /** A one-off pane: a shell in a container, a restart, a stack stopping. */
  run(req: DockerRun): void;
}

export interface DockerPageProps {
  /** Every daemon and every workspace — the stacks are *a machine's*, see `stacksFor`. */
  world: World;
  /** The current workspace, already qualified, or null when none is open. */
  ws: QualifiedWorkspace | null;
  /**
   * The follower's pane, qualified, or null while nothing is being followed.
   *
   * The shell's answer rather than the page's, because *whose* workspace a
   * follower is spawned in is the shell's question — see `followerPane` for the
   * lookup a shell that spawns it here can use.
   */
  pane: Qid | null;
  /** What a `"default"` cell resolves to. `<Stage>` cannot read it off the page. */
  theme: TermTheme;
  /// The stage's cell size in CSS pixels. Omitted leaves the renderer's own.
  fontPx?: number | undefined;
  /// The stage's own events — a bell, a refused pane, a daemon whose version
  /// disagrees with this client's. The page only forwards them: dropping a
  /// refused pane is the shell's call, and this is how it hears about one.
  stage?: StageEvents | undefined;
  actions: DockerActions;
}

/** The name every follower pane is given. A prefix, so the shell can reap by it. */
export const FOLLOWER = "logs:";

/** The process name a row's follower takes. */
export function followerName(key: string): string {
  return FOLLOWER + key;
}

/**
 * The follower pane in this workspace, or null.
 *
 * Exported because the shell needs it and the naming rule is this page's: the
 * page asked for a pane called `logs:<key>`, so the page is what knows how to
 * find it again. A shell that spawns followers somewhere else answers `pane`
 * its own way and never calls this.
 */
export function followerPane(ws: QualifiedWorkspace | null, name: string): Qid | null {
  return ws?.processes.find((p) => p.name === name)?.pane ?? null;
}

/**
 * The stacks to draw: **the workspace's own machine's**.
 *
 * `world.system` is the *primary* daemon's telemetry, so a client with two
 * daemons that read it would list one machine's containers beside another
 * machine's workspace — and every verb here would then run `docker restart` on
 * the wrong host, successfully. With no workspace open there is no such
 * question, and the primary's list is the only answer there is.
 */
export function stacksFor(world: World, ws: QualifiedWorkspace | null): readonly StackDto[] {
  const daemon = ws?.daemon ?? null;
  const entry = daemon == null ? null : world.daemons.find((d) => d.key === daemon);
  const system = entry ? entry.system : world.system;
  return system?.stacks ?? [];
}

// The arrows are the same verbs `j` and `k` are, spelled the way the browser
// spells them. `verbs.ts` holds the pair once; this is the second spelling and
// the only place it exists.
const ARROW: Readonly<Record<string, VerbId | undefined>> = {
  arrowdown: VerbId.Down,
  arrowup: VerbId.Up,
};

/**
 * True when `path` is `base` or lives inside it. The boundary matters: a bare
 * `startsWith` calls `/home/u/app2` a child of `/home/u/app`.
 */
function under(path: string, base: string): boolean {
  if (path === base) return true;
  return path.startsWith(base.endsWith("/") ? base : base + "/");
}

/** Single-quote a path for safe use in a shell command. */
function shq(s: string): string {
  return "'" + s.replace(/'/g, "'\\''") + "'";
}

/**
 * A compose stack "belongs" to the current workspace when its working dir is at,
 * under or over the workspace's cwd — the daemon's own `mine` heuristic, and the
 * reason a project's own containers sort to the top of a list that is otherwise
 * every container on the machine.
 */
function mine(s: StackDto, cwd: string): boolean {
  if (!s.workdir || !cwd) return false;
  return under(cwd, s.workdir) || under(s.workdir, cwd);
}

/** One drawn row: a stack, or a container under (or standing in for) one. */
export interface DockerLine {
  kind: DockerRow;
  key: string;
  stack: StackDto;
  container: ContainerDto | null;
  indent: boolean;
}

/**
 * The list, flattened: one entry per drawn row, in drawn order.
 *
 * The cursor is an index into *this*, not into the stacks — a stack with four
 * containers is five rows, and a cursor that counted stacks would skip four of
 * them. It is also what makes `j`/`k` and the pointer agree about what row 7 is,
 * which is the property the whole keyboard layer rests on.
 */
export function listRows(stacks: readonly StackDto[], cwd: string): DockerLine[] {
  const sorted = [...stacks].sort((a, b) => Number(mine(b, cwd)) - Number(mine(a, cwd)));
  const out: DockerLine[] = [];
  for (const s of sorted) {
    // A one-member stack is drawn as its container, because
    // `docker compose logs` in a directory that has no compose file is an error
    // message where the logs should be. `total` is the daemon's count and
    // `containers` is the list it counted; when they disagree the list wins,
    // since a row with no container behind it has nothing to act on.
    const only = s.total === 1 ? (s.containers[0] ?? null) : null;
    out.push({
      kind: only ? DockerRow.Container : DockerRow.Stack,
      key: only ? "cont:" + only.name : "stack:" + s.label,
      stack: s,
      container: only,
      indent: false,
    });
    if (only) continue;
    for (const c of s.containers) {
      out.push({ kind: DockerRow.Container, key: "cont:" + c.name, stack: s, container: c, indent: true });
    }
  }
  return out;
}

const up = (c: ContainerDto | null): boolean => c != null && c.state === "running";

/** Whether a key event came from somewhere a bare letter is a letter. */
function isTyping(t: EventTarget | null): boolean {
  const el = t as HTMLElement | null;
  if (!el?.tagName) return false;
  const tag = el.tagName.toLowerCase();
  return tag === "input" || tag === "textarea" || tag === "select" || el.isContentEditable;
}

/**
 * The click registry, in React's spelling.
 *
 * `verbs.ts`'s `click()` throws for a target that is not declared in `TARGETS`,
 * which is what stops a button existing with no key that reaches it; its own
 * return is `h()`'s spelling, so the assertion is what is kept. `data-verb` is
 * how `keys.ts` finds the thing a verb clicks — the vanilla page carried none on
 * this surface, which is why its rows could not be reached that way at all.
 */
function verbClick(target: TargetId, run: () => void) {
  click(target, run);
  return { "data-verb": target, onClick: run };
}

/** The same assertion for a `Row`, which composes its own activation — see FilesPage. */
function verbTarget(target: TargetId, run: () => void) {
  click(target, run);
  return { "data-verb": target };
}

// ---------------------------------------------------------------------------
// The page
// ---------------------------------------------------------------------------

export function DockerPage({ world, ws, pane, theme, fontPx, stage, actions }: DockerPageProps) {
  const cwd = ws?.cwd ?? "";
  const stacks = stacksFor(world, ws);
  const rows = useMemo(() => listRows(stacks, cwd), [stacks, cwd]);
  const [sel, setSel] = useState<string | null>(null);
  const [cursor, setCursor] = useState(0);
  const at = Math.max(0, Math.min(rows.length - 1, cursor));
  const bar = rows.find((r) => r.key === sel) ?? null;
  /** The row a verb acts on: the bar's, or the cursor's while nothing is followed yet. */
  const target = bar ?? rows[at] ?? null;
  // Contextual on the row the *cursor* is on, which is what the terminal does: a
  // stack and a container answer to the same letters and mean different things
  // by them, so the footer has to change as you walk past the boundary rather
  // than when you press enter.
  const verbs = dockerVerbs(rows[at]?.kind ?? DockerRow.None);
  const total = stacks.reduce((n, s) => n + s.total, 0);

  // The cursor is focus, so moving it moves the browser's. Only after the
  // keyboard has actually been used: stealing focus on load would scroll a page
  // the reader has not asked to move.
  const listEl = useRef<HTMLDivElement | null>(null);
  const walked = useRef(false);
  useEffect(() => {
    if (!walked.current || !listEl.current) return;
    const el = listEl.current.querySelector<HTMLElement>(`[data-row="${at}"]`);
    if (el && el !== document.activeElement) el.focus();
  }, [at]);

  /**
   * Follow a row's logs. A whole compose project when the row is a stack, one
   * container when it is not.
   */
  const follow = useCallback(
    (r: DockerLine | undefined | null) => {
      if (!r) return;
      setSel(r.key);
      if (r.container) {
        actions.logs({
          name: followerName(r.container.name),
          command: `docker logs -f --tail 200 ${r.container.name}`,
        });
        return;
      }
      const cd = r.stack.workdir ? `cd ${shq(r.stack.workdir)} && ` : "";
      actions.logs({ name: followerName(r.stack.label), command: `${cd}docker compose logs -f --tail 200` });
    },
    [actions],
  );

  const select = useCallback(
    (i: number) => {
      setCursor(i);
      follow(rows[i]);
    },
    [rows, follow],
  );

  // The three action verbs, all of them on the row the *bar* is showing.
  const contAct = useCallback(
    (kind: "shell" | "restart", name: string) => {
      actions.run({
        name: `${kind}:${name}`,
        command: kind === "shell" ? `docker exec -it ${name} sh` : `docker restart ${name}`,
      });
    },
    [actions],
  );

  const composeAct = useCallback(
    (s: StackDto, sub: "restart" | "stop") => {
      const cd = s.workdir ? `cd ${shq(s.workdir)} && ` : "";
      actions.run({ name: `${sub}:${s.label}`, command: `${cd}docker compose ${sub}` });
    },
    [actions],
  );

  /**
   * One dispatch for the keyboard and for the footer's buttons, so a hint you
   * click runs the verb its key runs. Returns whether the verb was ours: a key
   * this page does not act on has to reach the page under it rather than being
   * swallowed by a `preventDefault` for nothing.
   */
  const run = useCallback(
    (id: VerbId): boolean => {
      if (id === VerbId.Down) {
        walked.current = true;
        setCursor(Math.min(rows.length - 1, at + 1));
        return true;
      }
      if (id === VerbId.Up) {
        walked.current = true;
        setCursor(Math.max(0, at - 1));
        return true;
      }
      if (id === VerbId.DockerLogs) {
        follow(rows[at]);
        return true;
      }
      if (!target) return false;
      if (id === VerbId.DockerShell && target.container) {
        contAct("shell", target.container.name);
        return true;
      }
      if (id === VerbId.DockerRestart) {
        if (target.container) contAct("restart", target.container.name);
        else composeAct(target.stack, "restart");
        return true;
      }
      if (id === VerbId.DockerStop && !target.container) {
        composeAct(target.stack, "stop");
        return true;
      }
      return false;
    },
    [rows, at, target, follow, contAct, composeAct],
  );

  // No dependency array: the listener closes over the cursor, and one registered
  // on mount would go on acting on row 0 forever.
  //
  // This is the half of the page the shell is expected to take over — every row
  // and every button below carries its `data-verb`, which is how `keys.ts`
  // reaches them — and until there is a shell, a page that advertises `r
  // restart` in its own footer has to answer for it.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.defaultPrevented || e.altKey || e.ctrlKey || e.metaKey) return;
      // The vanilla page stood alone and had nothing to type into. This one is
      // mounted inside a shell that has a command palette and a dialog, and
      // `r` typed into either of them must be an `r` — not a container
      // restarting behind the box you are typing in.
      if (isTyping(e.target)) return;
      const name = keyName(e);
      const verb = verbs.find((v) => v.key === name);
      const id = verb ? verb.id : ARROW[name];
      if (id && run(id)) e.preventDefault();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  });

  // `parts.ts`'s packer rather than a second copy of it: every verb this footer
  // draws is one this page can run on the row it is showing, so "which verbs are
  // worth a column" is the only question left and `fits` is where that is
  // answered — at the terminal's own column count, or the two clients teach
  // different keys. FILES cannot use this and says why.
  const press = useCallback(
    (key: string) => {
      const v = verbs.find((x) => x.key === key);
      if (v) run(v.id);
    },
    [verbs, run],
  );
  const keys: Hint[] = hints(verbs, RAIL_COLS, MAX_ROWS, press);

  return (
    <div className="flex h-full min-h-0 flex-col bg-background">
      <div className="flex min-h-0 flex-1">
        <aside className="flex w-72 shrink-0 flex-col border-r border-border bg-card">
          <SectionTitle action={<Badge variant="outline">{total}</Badge>}>docker</SectionTitle>
          <ScrollArea type="auto" className="min-h-0 flex-1">
            {rows.length === 0 ? (
              <Empty>no containers</Empty>
            ) : (
              <div role="listbox" aria-label="docker" className="flex flex-col" ref={listEl}>
                {rows.map((r, i) => (
                  <DockerListRow
                    key={r.key + "/" + i}
                    line={r}
                    index={i}
                    cwd={cwd}
                    selected={r.key === sel}
                    onSelect={() => select(i)}
                  />
                ))}
              </div>
            )}
          </ScrollArea>
        </aside>

        <main className="flex min-w-0 flex-1 flex-col">
          <SectionTitle
            action={
              bar ? (
                <>
                  {bar.container ? (
                    <Button
                      size="sm"
                      variant="outline"
                      title="A shell in this container (s)"
                      {...verbClick("docker.shell", () => bar.container && contAct("shell", bar.container.name))}
                    >
                      shell
                    </Button>
                  ) : null}
                  <Button
                    size="sm"
                    variant="outline"
                    title={bar.container ? "Restart it (r)" : "Restart every container in the stack (r)"}
                    {...verbClick("docker.restart", () =>
                      bar.container ? contAct("restart", bar.container.name) : composeAct(bar.stack, "restart"),
                    )}
                  >
                    restart
                  </Button>
                  {bar.container ? null : (
                    <Button
                      size="sm"
                      variant="destructive"
                      title="Stop the stack (x)"
                      {...verbClick("docker.stop", () => composeAct(bar.stack, "stop"))}
                    >
                      stop
                    </Button>
                  )}
                </>
              ) : null
            }
          >
            {/* A container's name is an identifier — you type it at a shell —
                so it keeps its own case and its own font inside a header whose
                voice is caps. Same rule as the path in FILES' header: a name
                read in capitals is a different name. */}
            {bar ? (
              <>
                {bar.container ? "logs: " : "compose: "}
                <span className="font-mono tracking-normal text-foreground normal-case">
                  {bar.container ? bar.container.name : bar.stack.label}
                </span>
              </>
            ) : (
              "logs"
            )}
          </SectionTitle>
          {/* Three states, not two. `<Stage>`'s own empty text is "select an
              agent or process", which is the right sentence on WORK and the
              wrong one here — so the page says its own thing while the follower
              is being spawned, and the stage is mounted only once there is a
              pane for it to attach to. The cost is a socket per switch rather
              than a `watch` re-point; a follower switch already costs a process
              spawn, so it is not the expensive half. */}
          {!bar ? (
            <Empty className="h-auto min-h-0 flex-1 justify-center">
              Select a stack or container to follow its logs.
            </Empty>
          ) : pane == null ? (
            <Empty className="h-auto min-h-0 flex-1 justify-center">starting the log follower…</Empty>
          ) : (
            <Stage
              pane={pane}
              theme={theme}
              className="min-h-0 flex-1"
              {...(fontPx != null ? { fontPx } : {})}
              {...(stage ?? {})}
            />
          )}
        </main>
      </div>

      <HintBar keys={keys} />
    </div>
  );
}

// ---------------------------------------------------------------------------
// One row
// ---------------------------------------------------------------------------

interface DockerListRowProps {
  line: DockerLine;
  index: number;
  cwd: string;
  selected: boolean;
  onSelect: () => void;
}

function DockerListRow({ line, index, cwd, selected, onSelect }: DockerListRowProps) {
  const c = line.container;
  const running = up(c);
  return (
    <Row
      selected={selected}
      onSelect={onSelect}
      data-row={index}
      title={c ? `${c.name} (${c.state})` : line.stack.project ? `compose: ${line.stack.project}` : line.stack.label}
      {...verbTarget(c ? "docker.container" : "docker.stack", onSelect)}
    >
      {line.indent ? <span aria-hidden="true" className="w-3 shrink-0" /> : null}
      <span
        aria-hidden="true"
        className={
          "w-3 shrink-0 text-center " + (c ? (running ? "text-ok" : "text-dim") : "text-primary")
        }
      >
        {c ? "●" : "▾"}
      </span>
      {/* A container's name is a thing you type at a shell, so it is mono; a
          compose project is a label, and one of ours is drawn in the brand
          colour for the same reason the daemon sorts it first. */}
      <span
        className={
          "min-w-0 flex-1 truncate font-mono " +
          (!line.indent && !c ? "font-semibold " : "") +
          (mine(line.stack, cwd) ? "text-primary" : "")
        }
      >
        {c ? c.name : line.stack.label}
      </span>
      {/* `Badge` has no palette tones, so a running container is an outline
          badge wearing `--ok` — one colour named, no literal, and `3/4 up` and
          `exited (1)` still read as the same kind of thing at the same size. */}
      {c ? (
        <Badge variant="outline" className={running ? "border-ok/40 text-ok" : "text-dim"}>
          {c.state}
        </Badge>
      ) : (
        <Badge
          variant="outline"
          className={"tabular-nums " + (line.stack.running > 0 ? "border-ok/40 text-ok" : "text-dim")}
        >
          {line.stack.running + "/" + line.stack.total + " up"}
        </Badge>
      )}
    </Row>
  );
}
