// Everything that writes.
//
// One place, for the reason `world.ts` is one place: a page that called the
// daemon directly would be a second door, and the first one nobody checks when a
// call goes wrong. Every method here reports through a toast and returns — **no
// method throws**, so a page never needs a `try`.
//
// ## What a git call answering 202 means
//
// A git operation answers 200 when it finishes inside the daemon's grace window
// and **202 when it does not**, so `running` is a normal answer rather than an
// error, and the event stream is what shows the result of the ones that keep
// going. Separately, `ok: false` is a *successful call reporting a failed
// operation* — a rejected push is the common case — so it is read rather than
// thrown.
//
// ## Confirmation is a property of the operation, not of the caller
//
// `discard` and `reset --hard` are destructive and unrecoverable; the daemon's
// own row says so. They ask here, once, rather than in each page that offers
// them — which is how the old client ended up with a route it could reach and no
// user able to get to it.
//
// ## The question is a property of the operation too
//
// Half the verbs below need an argument the button that starts them does not
// carry: *which* agent type, *which* branch, what to call a process. `web/ui/`
// answered that with an `askFor` inside its dispatch, and it is the same
// argument as the confirmation — a page that asked would own a dialog, and six
// pages would own six. So `o.ask` is the shell's one question, `o.confirm` is
// the shell's one warning, and `o.patch` is the shell's one reader, and the
// verbs here compose them.
//
// ## What is *not* here
//
// A cursor. Which pane is on the stage, which rail has the keyboard, which topic
// HELP has open, whether the rail drawer is over the stage — none of that
// reaches a daemon, so none of it is a write, and all of it lives in
// `Shell.tsx`. The one seam where the two meet is `pages.tsx`, which is also
// where a footer key becomes a verb: `press` needs both a cursor and a verb, and
// it is the only thing on a page's actions interface that is not a call.

import { toast } from "sonner";
import { api } from "../logic/api.ts";
import { daemonOf, qid, type Qid } from "../logic/events.ts";
import { GitAction, type GitActionId } from "../logic/git-menu.ts";
import type { ResetMode, ResolveSide, SequenceAction, WorktreeDto } from "../protocol/generated/protocol.ts";

// `api.ts` takes a qualified id as a `string`; `Qid` is `string | number`
// because a single-daemon reply may still carry a bare integer. One
// conversion here beats one at every call site.
const q = (id: Qid): string => String(id);

const why = (e: unknown): string => (e instanceof Error ? e.message : String(e));

/** The last segment of a path — what a worktree is called, for a toast. */
const base = (path: string): string => path.replace(/\/+$/, "").split("/").pop() || path;

/**
 * One field of the shell's question.
 *
 * `options` is what makes it a choice rather than free text, and the shape is
 * `web/ui/actions.js`'s `askFor` field exactly: the dispatch there had the same
 * two kinds and no more, across every verb that needed an argument.
 */
export interface AskField {
  label: string;
  value?: string | undefined;
  options?: readonly string[] | undefined;
}

export interface ActionsOptions {
  /** Re-read `/api/state` now. An action calls it so a row moves immediately. */
  refresh: () => void;
  /** Ask before something unrecoverable. Returns whether to go ahead. */
  confirm: (question: string, detail?: string) => Promise<boolean>;
  /**
   * Ask for the argument the button did not carry. One value per field, in
   * order, or `null` when it was cancelled — which every caller reads as "then
   * do nothing".
   */
  ask: (title: string, fields: readonly AskField[], submit?: string) => Promise<string[] | null>;
  /** Show a patch, read-only. A diff and a commit are the same reader. */
  patch: (title: string, text: string) => void;
  /**
   * Whether any verb is in flight, so a page can disable the row that started
   * it. Reported rather than exposed as the `busy` set below, because a `Set`
   * a component mutates is a `Set` no component re-renders for.
   */
  onBusy?: ((busy: boolean) => void) | undefined;
}

/**
 * What every call reports, whether it worked or not.
 *
 * Structural rather than a union of the DTOs: a git route answers `GitOpDto`, a
 * staging route answers `OkReply` and the roster answers `DaemonDto`, and the
 * only fields this cares about are the three they happen to share. Naming them
 * all here would mean editing this file every time a route is added, for no
 * gain — `ok`, `running` and `summary` are the whole vocabulary of a toast.
 */
type Reply =
  | { ok?: boolean | null | undefined; running?: boolean | undefined; summary?: string | undefined }
  | null
  | undefined;

export class Actions {
  /** Verbs currently in flight, so a page can disable a row's button. */
  busy = new Set<string>();

  constructor(private readonly o: ActionsOptions) {}

  /**
   * One verb, run and reported.
   *
   * `label` is what the toast says, so it is written as the user would name the
   * thing they just did — "stage src/main.rs", not "POST changes/stage".
   *
   * `read` is the one arm that reports nothing on success, and it is not an
   * exception to the rule so much as the rule applied: a read moved nothing, so
   * there is nothing to say and nothing to re-read — the overlay appearing *is*
   * the answer. A failure still comes back through here, which is the whole
   * reason a read goes through it at all.
   */
  private async run(label: string, key: string, call: () => Promise<Reply>, read = false): Promise<Reply> {
    this.busy.add(key);
    this.o.onBusy?.(true);
    try {
      const r = await call();
      if (read) return r;
      if (r && r.running) toast.loading(`${label}: running…`, { id: key, duration: 4000 });
      // `ok === false` and `ok == null` are different answers. A `GitOpDto`
      // carries `ok: null` while the operation is still running — no verdict
      // yet — and treating that as failure would report every slow fetch as a
      // rejected one.
      else if (r && r.ok === false) toast.error(`${label}: ${r.summary || "refused"}`);
      else toast.success(`${label}: ${(r && r.summary) || "done"}`);
      this.o.refresh();
      return r;
    } catch (e) {
      toast.error(`${label}: ${why(e)}`);
      return null;
    } finally {
      this.busy.delete(key);
      this.o.onBusy?.(this.busy.size > 0);
    }
  }

  /**
   * Ask which one, out of a list the daemon answers.
   *
   * The list is read *before* the question, so "no branches" and "no stashes"
   * are said instead of drawn as an empty picker — the failure the old client
   * had was a dialog with one blank row in it and no way to tell whether that
   * meant none or meant broken.
   */
  private async choose(
    title: string,
    label: string,
    load: () => Promise<readonly string[]>,
    submit: string,
    dflt?: string | null,
  ): Promise<string | null> {
    let names: readonly string[] = [];
    try {
      names = await load();
    } catch (e) {
      toast.error(`${label}s: ${why(e)}`);
      return null;
    }
    if (!names.length) {
      toast.message(`no ${label}s`);
      return null;
    }
    const first = dflt && names.includes(dflt) ? dflt : (names[0] ?? "");
    const r = await this.o.ask(title, [{ label, value: first, options: names }], submit);
    const picked = r?.[0]?.trim();
    return picked ? picked : null;
  }

  /** The daemon a qualified id names, or a throw that `run` turns into a toast. */
  private static daemon(id: Qid): string {
    const d = daemonOf(id);
    if (!d) throw new Error(`${JSON.stringify(String(id))} is not qualified — ids are <daemon>:<id>`);
    return d;
  }

  // ---- reporting ------------------------------------------------------------

  /**
   * Re-read `/api/state` now.
   *
   * `r refresh` is a verb in the CHANGES table, so it needs to be reachable the
   * way every other verb is. It is the one member here that reads rather than
   * writes and reports nothing: the rails redrawing *is* the answer, and a toast
   * saying "refreshed" after a poll that changed nothing is noise.
   */
  refresh = (): void => {
    this.o.refresh();
  };

  /**
   * A page's own refusal, said in the one place messages are said.
   *
   * FILES needs it for the two things it will not do — a built-in reference page
   * is not a file on disk, and a 4MB file is not something to open in a
   * textarea. Neither is the outcome of a call, so neither has a `run` to report
   * it, and a page that reached for `sonner` itself would be the second toaster.
   */
  toast = (message: string): void => {
    toast.warning(message);
  };

  /**
   * Put text on the clipboard.
   *
   * A write, and one that fails in a way worth reporting: `navigator.clipboard`
   * is undefined outside a secure context, so a bridge reached over plain HTTP
   * on another host has no clipboard at all. Through `run` like everything else,
   * which is what turns that into a message instead of a rejected promise
   * nobody is listening to.
   */
  copySha = (sha: string): Promise<Reply> =>
    this.run(`copied ${sha.slice(0, 8)}`, `copy:${sha}`, async () => {
      if (!navigator.clipboard) throw new Error("no clipboard — this page is not on a secure origin");
      await navigator.clipboard.writeText(sha);
      return { ok: true, summary: sha.slice(0, 8) };
    });

  // ---- agents and processes -------------------------------------------------

  spawn = (ws: Qid, kind: string) => this.run(`new ${kind}`, `spawn:${ws}`, () => api.spawnAgent(q(ws), kind));

  /**
   * Spawn an agent, asking which kind unless SETTINGS has pinned one.
   *
   * The types are the *machine's* answer, not a global list: what runs on the
   * gpu box is what is installed on the gpu box. So a pin naming something this
   * machine does not have is not a reason to spawn the wrong thing — the pin is
   * one browser's and the types are one daemon's, they can honestly disagree,
   * and asking is the safe answer. `choose` is `A`, which asks whatever the pin
   * says.
   */
  spawnPick = async (ws: Qid, choose: boolean, pin: string | null): Promise<Reply> => {
    const d = daemonOf(ws);
    const types = d ? await api.agentTypes(d) : [];
    const list = types.length ? types : ["claude"];
    if (!choose && pin && list.includes(pin)) return this.spawn(ws, pin);
    if (!choose && pin) toast.info(`${pin} is not installed on this machine — pick one`);
    const kind = await this.choose("New agent", "type", async () => list, "Start", pin);
    return kind ? this.spawn(ws, kind) : null;
  };

  /**
   * Mark an agent read without attaching.
   *
   * The daemon clears `unread` wherever a client *looks* at a pane — staging,
   * watching, streaming, sending input — and this is the fifth of those, for
   * the case where you have read a row in a rail and are not going to open it.
   */
  ack = (ws: Qid, pane: Qid) => this.run("ack", `ack:${pane}`, () => api.ackPane(q(ws), q(pane)));

  kill = async (ws: Qid, pane: Qid, what: string) => {
    if (!(await this.o.confirm(`End ${what}?`, "The pane and its scrollback go with it."))) return null;
    return this.run(`end ${what}`, `kill:${pane}`, () => api.killPane(q(ws), q(pane)));
  };

  restart = (ws: Qid, pane: Qid, name: string) =>
    this.run(`restart ${name}`, `restart:${pane}`, () => api.restartProcess(q(ws), q(pane)));

  /** A shell, or anything else worth supervising. `t` on the PROCESSES rail. */
  newProc = async (ws: Qid): Promise<Reply> => {
    const r = await this.o.ask(
      "New process",
      [{ label: "name", value: "term" }, { label: "command", value: "$SHELL" }],
      "Start",
    );
    const name = r?.[0]?.trim();
    if (!name) return null;
    const command = r?.[1]?.trim() || "$SHELL";
    return this.run(`start ${name}`, `proc:${name}`, () => api.newProcess(q(ws), name, command));
  };

  /**
   * Answer an agent without attaching to it.
   *
   * `POST .../panes/{pane}/input` — one of the routes no browser client has ever
   * reached. "Answer this agent yes" from a rail is the whole point of it.
   */
  send = (ws: Qid, pane: Qid, text: string) =>
    this.run("sent", `input:${pane}`, () => api.paneInput(q(ws), q(pane), text));

  // ---- the working tree -----------------------------------------------------

  stage = (ws: Qid, path: string) => this.run(`stage ${path}`, `stage:${path}`, () => api.stage(q(ws), path));
  unstage = (ws: Qid, path: string) => this.run(`unstage ${path}`, `unstage:${path}`, () => api.unstage(q(ws), path));

  /**
   * Throw away a file's uncommitted changes.
   *
   * Declared in `api.ts` since the beginning and **bound to no verb**, so no
   * user could ever reach it — the daemon's own row calls it "destructive and
   * unrecoverable", which is a decision the old client never made. It is made
   * here: it asks, and it names the file it is about to lose.
   */
  discard = async (ws: Qid, path: string) => {
    const ok = await this.o.confirm(`Discard changes to ${path}?`, "This cannot be undone — the edits are not anywhere else.");
    return ok ? this.run(`discard ${path}`, `discard:${path}`, () => api.discard(q(ws), path)) : null;
  };

  commit = (ws: Qid, message: string, all = false) =>
    this.run("commit", "commit", () => (all ? api.commitAll(q(ws), message) : api.commit(q(ws), message)));

  /** `C` — stage everything, then commit. Its own method because the page hands
   * it around as a value: `(all ? actions.commitAll : actions.commit)(msg)`. */
  commitAll = (ws: Qid, message: string) => this.commit(ws, message, true);

  /**
   * Commit, asking for the message.
   *
   * The CHANGES rail has a message field of its own and this is not it: the
   * footer's `c` is out of reach of that field — it is inside `ChangesRail`, and
   * the bar spans the page — so pressing it asks rather than committing an empty
   * message. `web/ui/actions.js`'s `press` made the same call for the same
   * reason.
   */
  commitAsk = async (ws: Qid, all: boolean): Promise<Reply> => {
    const r = await this.o.ask(all ? "Stage everything, then commit" : "Commit", [{ label: "message" }], "Commit");
    const message = r?.[0]?.trim();
    return message ? this.commit(ws, message, all) : null;
  };

  /** Settle one conflicted file: take ours, take theirs, or "I have edited it". */
  resolve = (ws: Qid, path: string, take: ResolveSide) =>
    this.run(`${take} ${path}`, `resolve:${path}`, () => api.resolve(q(ws), path, take));

  /**
   * Drive whatever merge, rebase, cherry-pick or revert is stopped.
   *
   * `abort` asks first, and it is the one confirmation the old client did not
   * have: aborting discards every conflict you have already resolved, which can
   * be an afternoon, and `y`/`n` sit one key apart in the same footer.
   */
  sequence = async (ws: Qid, action: SequenceAction): Promise<Reply> => {
    if (action === "abort" && !(await this.o.confirm("Abort this operation?", "Everything resolved so far goes with it."))) {
      return null;
    }
    return this.run(action, `sequence:${action}`, (): Promise<Reply> => api.sequence(q(ws), action));
  };

  /** Read a file's diff into the shell's reader. Staged is index-vs-HEAD. */
  openDiff = (ws: Qid, what: { path: string; staged: boolean }) =>
    this.run(what.path, `diff:${what.path}`, async () => {
      const d = await api.diff(q(ws), what.path, what.staged);
      this.o.patch(what.path, d.patch || "(no changes)");
      return { ok: true };
    }, true);

  /** The same reader, whole-commit. `git show`, straight from the daemon. */
  showCommit = (ws: Qid, id: string) =>
    this.run(`commit ${id}`, `show:${id}`, async () => {
      const d = await api.show(q(ws), id);
      this.o.patch(`commit ${id}`, d.patch || "(empty)");
      return { ok: true };
    }, true);

  // ---- files ----------------------------------------------------------------

  /**
   * Write bytes to a path under the workspace's cwd.
   *
   * The one action that answers a boolean, and FILES needs it to: a save that
   * failed must leave the editor open with the draft still in it, and
   * fire-and-forget cannot say which happened. The toast is still `run`'s.
   */
  upload = async (ws: Qid, req: { path: string; blob: Blob }): Promise<boolean> => {
    const r = await this.run(`save ${req.path}`, `upload:${req.path}`, () => api.upload(q(ws), req.path, req.blob));
    return !!r && r.ok !== false;
  };

  /**
   * Delete a file off disk.
   *
   * Asks, for the same reason `discard` does and with more of it: `discard` is
   * bounded by what git already has, and this is not bounded by anything. It
   * answers a boolean like `upload` — the page has to know whether the row it
   * is showing still exists before it re-reads the listing.
   */
  deleteFile = async (ws: Qid, path: string): Promise<boolean> => {
    const ok = await this.o.confirm(`Delete ${path}?`, "This cannot be undone — the file is not in the trash or the index.");
    if (!ok) return false;
    const r = await this.run(`delete ${path}`, `delete:${path}`, () => api.deleteFile(q(ws), path));
    return !!r && r.ok !== false;
  };

  // ---- docker ---------------------------------------------------------------

  /**
   * Follow a stack or a container, and reap whatever was being followed.
   *
   * Both halves are one `run`, so it is one message rather than two: the reap is
   * not a thing the user did, it is the cost of the thing they did. `catch` on
   * the kill because a follower that has already exited is not a failure to
   * report — the pane being gone is the outcome that was wanted.
   *
   * The name is `logs:<key>`, which is DOCKER's own naming rule, and it reads
   * back as "logs web" in the message.
   */
  dockerLogs = (ws: Qid, req: { name: string; command: string }, reap: readonly Qid[]) =>
    this.run(req.name.replace(":", " "), `docker:${req.name}`, async () => {
      for (const pane of reap) await api.killPane(q(ws), q(pane)).catch(() => null);
      return api.newProcess(q(ws), req.name, req.command);
    });

  /** A one-off pane: a shell in a container, a restart, a stack stopping. */
  dockerRun = (ws: Qid, req: { name: string; command: string }) =>
    this.run(req.name.replace(":", " "), `docker:${req.name}`, () => api.newProcess(q(ws), req.name, req.command));

  /**
   * Stop a follower, without asking.
   *
   * `kill` confirms, and it is right to: ending an agent loses its scrollback.
   * A `docker logs -f` follower is the client's own plumbing — it was started by
   * clicking a row and it has nothing in it that is not still in docker — so a
   * dialog on the way out of the page would be a question about a decision
   * nobody made.
   */
  stopLogs = (ws: Qid, pane: Qid) =>
    this.run("stopped following", `docker:stop:${pane}`, () => api.killPane(q(ws), q(pane)));

  // ---- remotes --------------------------------------------------------------

  fetch = (ws: Qid) => this.run("fetch", "fetch", (): Promise<Reply> => api.fetch(q(ws), { prune: true }));
  pull = (ws: Qid) => this.run("pull", "pull", (): Promise<Reply> => api.pull(q(ws), {}));
  push = (ws: Qid) => this.run("push", "push", (): Promise<Reply> => api.push(q(ws), {}));

  /** One remote by name — the GIT page's REMOTES rows, where `f` is per row. */
  fetchRemote = (ws: Qid, remote: string) =>
    this.run(`fetch ${remote}`, `fetch:${remote}`, (): Promise<Reply> => api.fetch(q(ws), { remote, prune: true }));

  remoteAdd = async (ws: Qid): Promise<Reply> => {
    const r = await this.o.ask("Add a remote", [{ label: "name", value: "origin" }, { label: "url" }], "Add");
    const name = r?.[0]?.trim();
    const url = r?.[1]?.trim();
    if (!name || !url) return null;
    return this.run(`add ${name}`, `remote:${name}`, (): Promise<Reply> => api.remoteAdd(q(ws), name, url));
  };

  remoteRemove = async (ws: Qid): Promise<Reply> => {
    const name = await this.choose("Remove a remote", "remote", () => api.remotes(q(ws)).then((l) => l.map((x) => x.name)), "Remove");
    if (!name) return null;
    return this.run(`remove ${name}`, `remote:rm:${name}`, (): Promise<Reply> => api.remoteRemove(q(ws), name));
  };

  // ---- branches -------------------------------------------------------------

  /**
   * Switch to an existing branch.
   *
   * Creating one is deliberately *not* smuggled in here as "the name you typed
   * does not exist yet": a typo would silently become a branch, and the undo for
   * that is not obvious. `newBranch` is the verb that creates one, and the `g`
   * menu is where it lives.
   */
  branch = async (ws: Qid): Promise<Reply> => {
    const dto = await api.branches(q(ws)).catch((e: unknown) => {
      toast.error(`branches: ${why(e)}`);
      return null;
    });
    if (!dto) return null;
    const names = dto.branches ?? [];
    if (!names.length) {
      toast.message("no branches");
      return null;
    }
    const r = await this.o.ask(
      "Switch branch",
      [{ label: "branch", value: dto.current ?? names[0] ?? "", options: names }],
      "Checkout",
    );
    const name = r?.[0]?.trim();
    return name && name !== dto.current ? this.checkout(ws, name) : null;
  };

  checkout = (ws: Qid, branch: string) =>
    this.run(`checkout ${branch}`, `checkout:${branch}`, () => api.checkout(q(ws), branch));

  newBranch = async (ws: Qid): Promise<Reply> => {
    const r = await this.o.ask("New branch", [{ label: "name" }, { label: "from", value: "HEAD" }], "Create");
    const name = r?.[0]?.trim();
    if (!name) return null;
    const from = r?.[1]?.trim() || null;
    return this.run(`new branch ${name}`, `branch:${name}`, () => api.branchCreate(q(ws), name, from));
  };

  renameBranch = async (ws: Qid): Promise<Reply> => {
    const r = await this.o.ask("Rename the current branch", [{ label: "to" }], "Rename");
    const to = r?.[0]?.trim();
    if (!to) return null;
    return this.run(`rename to ${to}`, `branch:rename`, () => api.branchRename(q(ws), null, to));
  };

  /** Delete a branch. Asks, because unmerged work on it goes with it. */
  deleteBranch = async (ws: Qid, name: string): Promise<Reply> => {
    if (!(await this.o.confirm(`Delete branch ${name}?`, "Commits only on this branch become unreachable."))) return null;
    return this.run(`delete ${name}`, `branch:rm:${name}`, () => api.branchDelete(q(ws), name));
  };

  merge = (ws: Qid, branch: string) =>
    this.run(`merge ${branch}`, `merge:${branch}`, (): Promise<Reply> => api.merge(q(ws), branch));

  rebase = (ws: Qid, onto: string) =>
    this.run(`rebase onto ${onto}`, `rebase:${onto}`, (): Promise<Reply> => api.rebase(q(ws), onto));

  // ---- tags -----------------------------------------------------------------

  newTag = async (ws: Qid): Promise<Reply> => {
    const r = await this.o.ask("New tag", [{ label: "name" }, { label: "at", value: "HEAD" }], "Tag");
    const name = r?.[0]?.trim();
    if (!name) return null;
    return this.run(`tag ${name}`, `tag:${name}`, (): Promise<Reply> => api.tag(q(ws), name, r?.[1]?.trim() || null));
  };

  deleteTag = async (ws: Qid, name: string): Promise<Reply> => {
    if (!(await this.o.confirm(`Delete tag ${name}?`, "Local only — a tag already pushed stays on the remote."))) return null;
    return this.run(`delete ${name}`, `tag:rm:${name}`, (): Promise<Reply> => api.tagDelete(q(ws), name));
  };

  // ---- the stash ------------------------------------------------------------

  stash = (ws: Qid) =>
    this.run("stash", "stash", (): Promise<Reply> => api.stash(q(ws), { include_untracked: true }));

  stashPop = (ws: Qid, index: number) =>
    this.run(`pop stash@{${index}}`, `stash:pop:${index}`, (): Promise<Reply> => api.stashApply(q(ws), index, true));

  /** Asks which one — the `g` menu's "pop a specific one". */
  stashPick = async (ws: Qid): Promise<Reply> => {
    const picked = await this.choose("Pop a stash", "stash", () => this.stashNames(ws), "Pop");
    const i = indexOfStash(picked);
    return i == null ? null : this.stashPop(ws, i);
  };

  stashDrop = async (ws: Qid, index: number): Promise<Reply> => {
    if (!(await this.o.confirm(`Drop stash@{${index}}?`, "A dropped stash is not recoverable."))) return null;
    return this.run(`drop stash@{${index}}`, `stash:drop:${index}`, (): Promise<Reply> => api.stashDrop(q(ws), index));
  };

  stashDropPick = async (ws: Qid): Promise<Reply> => {
    const picked = await this.choose("Drop a stash", "stash", () => this.stashNames(ws), "Drop");
    const i = indexOfStash(picked);
    return i == null ? null : this.stashDrop(ws, i);
  };

  // `stash@{2} on main: wip` — the index has to survive the picker, so it is
  // the first thing in the row and `indexOfStash` reads it back.
  private stashNames = (ws: Qid): Promise<string[]> =>
    api.stashes(q(ws)).then((l) => l.map((s) => `stash@{${s.index}} ${s.branch}: ${s.message}`));

  // ---- history --------------------------------------------------------------

  revert = (ws: Qid, rev: string) =>
    this.run(`revert ${rev.slice(0, 8)}`, `revert:${rev}`, (): Promise<Reply> => api.revert(q(ws), rev));

  cherryPick = (ws: Qid, rev: string) =>
    this.run(`cherry-pick ${rev.slice(0, 8)}`, `pick:${rev}`, (): Promise<Reply> => api.cherryPick(q(ws), rev));

  amend = async (ws: Qid): Promise<Reply> => {
    const r = await this.o.ask("Amend the last commit", [{ label: "message (blank keeps it)" }], "Amend");
    if (r === null) return null;
    return this.run("amend", "amend", (): Promise<Reply> => api.amend(q(ws), r[0]?.trim() || null));
  };

  /**
   * Move HEAD. `hard` asks first — it is the one mode that discards the
   * worktree, which is `git-menu.ts`'s own reason for putting it behind a
   * confirmation.
   */
  reset = async (ws: Qid, rev: string, mode: ResetMode): Promise<Reply> => {
    if (mode === "hard" && !(await this.o.confirm(`Reset --hard to ${rev}?`, "Every uncommitted change in the tree is lost."))) {
      return null;
    }
    return this.run(`reset --${mode} ${rev}`, "reset", (): Promise<Reply> => api.reset(q(ws), rev, mode));
  };

  // ---- worktrees ------------------------------------------------------------

  /**
   * Open a worktree as a workspace, and answer which one it is.
   *
   * A worktree is a second checkout on another branch, and butai's model is
   * already one workspace per directory — so this is `newWorkspace` at that
   * path, and the caller switches to the id it answers. A worktree that already
   * *has* a workspace is not this method's business: going to an open tab is a
   * cursor move, and `pages.tsx` does it without asking a daemon anything.
   */
  openWorktree = async (ws: Qid, wt: WorktreeDto): Promise<Qid | null> => {
    let made: Qid | null = null;
    await this.run(`open ${base(wt.path)}`, `wt:${wt.path}`, async () => {
      const d = Actions.daemon(ws);
      const r = await api.newWorkspace(d, base(wt.path), wt.path);
      made = qid(d, r.id);
      return { ok: true, summary: base(wt.path) };
    });
    return made;
  };

  addWorktree = async (ws: Qid): Promise<Reply> => {
    const r = await this.o.ask("New worktree", [{ label: "path" }, { label: "branch (blank is HEAD)" }], "Create");
    const path = r?.[0]?.trim();
    if (!path) return null;
    const branch = r?.[1]?.trim() || null;
    return this.run(`worktree ${base(path)}`, `wt:add:${path}`, (): Promise<Reply> =>
      api.worktreeAdd(q(ws), path, branch, false));
  };

  removeWorktree = async (ws: Qid, path: string): Promise<Reply> => {
    if (!(await this.o.confirm(`Remove the worktree at ${path}?`, "git removes the checkout directory."))) return null;
    return this.run(`remove ${base(path)}`, `wt:rm:${path}`, (): Promise<Reply> => api.worktreeRemove(q(ws), path));
  };

  removeWorktreePick = async (ws: Qid): Promise<Reply> => {
    const path = await this.choose(
      "Remove a worktree",
      "worktree",
      () => api.worktrees(q(ws)).then((l) => l.filter((w) => !w.is_main).map((w) => w.path)),
      "Remove",
    );
    return path ? this.removeWorktree(ws, path) : null;
  };

  pruneWorktrees = (ws: Qid) =>
    this.run("prune worktrees", "wt:prune", (): Promise<Reply> => api.worktreePrune(q(ws)));

  /** The `g` menu's "open worktree…" — pick a path, then open it. */
  openWorktreePick = async (ws: Qid): Promise<Qid | null> => {
    let all: readonly WorktreeDto[] = [];
    const path = await this.choose(
      "Open a worktree",
      "worktree",
      () => api.worktrees(q(ws)).then((l) => { all = l; return l.map((w) => w.path); }),
      "Open",
    );
    const wt = all.find((w) => w.path === path);
    return wt ? this.openWorktree(ws, wt) : null;
  };

  // ---- the operation in flight ----------------------------------------------

  /**
   * Stop the git operation running in this workspace.
   *
   * `DELETE .../git/op`, which the daemon has served since 0.8 with no client
   * ever calling it — a fetch against a host that is not answering used to be
   * something you waited out.
   */
  cancelOp = (ws: Qid) => this.run("cancel", "op:cancel", (): Promise<Reply> => api.cancelGitOp(q(ws)));

  // ---- the machines list ----------------------------------------------------

  addDaemon = (socket: string, name?: string) =>
    this.run(`add ${name || socket}`, "daemon:add", async (): Promise<Reply> => {
      const d = await api.addDaemon({ socket, ...(name ? { name } : {}) });
      return { ok: !d.error, summary: d.error ?? d.label };
    });

  removeDaemon = async (key: string) => {
    if (!(await this.o.confirm(`Remove ${key} from this bridge?`, "Its projects leave the tab bar. The daemon keeps running."))) return null;
    return this.run(`remove ${key}`, `daemon:rm:${key}`, async (): Promise<Reply> => {
      const r = await api.removeDaemon(key);
      return { ok: true, summary: r.removed?.label ?? key };
    });
  };

  // ---- the `g` menu ---------------------------------------------------------

  /**
   * One row of `git-menu.ts`, run.
   *
   * The menu is a table and this is the other half of it: every `GitActionId`
   * has an arm here, which is what `check.py`'s `gitmenu/runnable` asks of the
   * terminal's menu and the same property this one keeps. A row that led
   * nowhere would be worse than a shorter menu — `git-menu.ts` says so in its
   * own header — so the switch is exhaustive rather than defaulted, and adding
   * a row to the table without a route behind it is a compile error here.
   *
   * `needsConfirm` is deliberately *not* consulted: the three actions it names
   * are `PushForce`, `ResetHard` and `SequenceAbort`, and all three confirm
   * inside the verb they call, where the question can name what it is about to
   * lose. Asking here as well would ask twice.
   */
  gitAction = (ws: Qid, action: GitActionId): Promise<unknown> => {
    switch (action) {
      case GitAction.Checkout: return this.branch(ws);
      case GitAction.NewBranch: return this.newBranch(ws);
      case GitAction.DeleteBranch: return this.deleteBranchPick(ws);
      case GitAction.RenameBranch: return this.renameBranch(ws);
      case GitAction.Fetch: return this.fetch(ws);
      case GitAction.Pull: return this.pull(ws);
      case GitAction.PullRebase:
        return this.run("pull --rebase", "pull", (): Promise<Reply> => api.pull(q(ws), { rebase: true }));
      case GitAction.Push: return this.push(ws);
      case GitAction.PushUpstream:
        return this.run("push -u", "push", (): Promise<Reply> => api.push(q(ws), { set_upstream: true }));
      case GitAction.PushForce: return this.pushForce(ws);
      case GitAction.RemoteAdd: return this.remoteAdd(ws);
      case GitAction.RemoteRemove: return this.remoteRemove(ws);
      case GitAction.StashPush: return this.stash(ws);
      case GitAction.StashPop: return this.stashPop(ws, 0);
      case GitAction.StashList: return this.stashPick(ws);
      case GitAction.StashDrop: return this.stashDropPick(ws);
      case GitAction.SequenceContinue: return this.sequence(ws, "continue");
      case GitAction.SequenceAbort: return this.sequence(ws, "abort");
      case GitAction.SequenceSkip: return this.sequence(ws, "skip");
      case GitAction.Merge: return this.mergePick(ws);
      case GitAction.Rebase: return this.rebasePick(ws);
      case GitAction.Amend: return this.amend(ws);
      case GitAction.ResetSoft: return this.reset(ws, "HEAD~1", "soft");
      case GitAction.ResetHard: return this.reset(ws, "HEAD", "hard");
      case GitAction.WorktreeList: return this.openWorktreePick(ws);
      case GitAction.WorktreeAdd: return this.addWorktree(ws);
      case GitAction.WorktreeRemove: return this.removeWorktreePick(ws);
      case GitAction.WorktreePrune: return this.pruneWorktrees(ws);
      case GitAction.TagCreate: return this.newTag(ws);
      case GitAction.TagDelete: return this.deleteTagPick(ws);
    }
  };

  /**
   * `--force-with-lease`, never a bare force — the protocol has no field for
   * one. It still asks: the lease refuses when the remote moved since you
   * fetched, and says nothing about commits you have already seen and are about
   * to drop.
   */
  private pushForce = async (ws: Qid): Promise<Reply> => {
    if (!(await this.o.confirm("Push --force-with-lease?", "It rewrites the remote branch. Anyone who pulled it has to recover."))) {
      return null;
    }
    return this.run("push --force-with-lease", "push", (): Promise<Reply> => api.push(q(ws), { force_with_lease: true }));
  };

  private branchNames = (ws: Qid): Promise<string[]> => api.branches(q(ws)).then((d) => d.branches ?? []);

  private deleteBranchPick = async (ws: Qid): Promise<Reply> => {
    const name = await this.choose("Delete a branch", "branch", () => this.branchNames(ws), "Delete");
    return name ? this.deleteBranch(ws, name) : null;
  };

  private mergePick = async (ws: Qid): Promise<Reply> => {
    const name = await this.choose("Merge a branch", "branch", () => this.branchNames(ws), "Merge");
    return name ? this.merge(ws, name) : null;
  };

  private rebasePick = async (ws: Qid): Promise<Reply> => {
    const onto = await this.choose("Rebase onto", "branch", () => this.branchNames(ws), "Rebase");
    return onto ? this.rebase(ws, onto) : null;
  };

  private deleteTagPick = async (ws: Qid): Promise<Reply> => {
    const name = await this.choose("Delete a tag", "tag", () => api.tags(q(ws)), "Delete");
    return name ? this.deleteTag(ws, name) : null;
  };
}

/** `stash@{2} …` back to `2`. Null when the picker was cancelled. */
function indexOfStash(row: string | null): number | null {
  const m = row ? /^stash@\{(\d+)\}/.exec(row) : null;
  return m?.[1] ? Number(m[1]) : null;
}
