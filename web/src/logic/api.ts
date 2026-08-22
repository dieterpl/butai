// REST client for the daemon's HTTP API, reached through the bridge's /api proxy.
// Read paths are plain GETs; actions are POST/DELETE. Everything rides Unix
// sockets behind the bridge — no daemon opens a port of its own.
//
// **Ids are qualified.** Every `id`/`pane` in here is a `<daemon>:<n>` string,
// not an integer: the bridge routes on the daemon in it and rewrites the segment
// to the bare number the daemon understands (`server/routing.ts`'s
// `resolveApiPath`). `ws()` below refuses a bare one before it leaves the
// browser, because the failure it prevents — a request landing on the wrong
// machine — produces a perfectly plausible answer rather than an error.
//
// Which is also why every id below is typed `string` and never `number`: the
// two are different types on purpose and unifying them is the bug.

import type {
  UsageDto,
  BranchesDto,
  BrowseDto,
  ConflictDto,
  DiffDto,
  FileDto,
  GitOp,
  GitOpDto,
  LogDto,
  NotificationsDto,
  RemoteDto,
  ResetMode,
  ResolveSide,
  SequenceAction,
  SessionId,
  StashDto,
  SysDto,
  TreeDto,
  WorkspaceDetail,
  WorktreeDto,
} from "../protocol/generated/protocol.ts";
// The bridge's own two routes have no generated binding — they are the
// bridge's, not the daemon's, and `ts-rs` only sees the Rust. Imported from
// where they are built rather than mirrored here: `import type` is erased
// (`verbatimModuleSyntax`), so this costs the browser nothing and a change to
// the reply is a compile error on both sides at once.
import type { DaemonDto } from "../../server/roster.ts";
import type { WorldState } from "../../server/snapshot.ts";
import type { QualifiedWorkspace } from "./events.ts";

/// The reply every action route sends: 200 `{"ok":true}`.
///
/// Not a generated DTO — `ApiReply::Ok` is built with `serde_json::json!` in
/// `http_conn.rs`, so `ts-rs` never sees it.
export interface OkReply {
  ok: boolean;
}

/// 201 `{"id":<n>}` — `ApiReply::Created`, the reply to creating a workspace.
export interface CreatedReply {
  id: SessionId;
}

/// The roster, straight off `GET /api/daemons`. No daemon is contacted for it.
export interface DaemonsReply {
  daemons: DaemonDto[];
}

/// The whole world, off `GET /api/state`.
///
/// The bridge's own `WorldState` with the two fields it leaves `unknown` said
/// properly, rather than a fourth spelling of the record: `snapshot.ts` builds
/// its workspaces by running the daemon's `WorkspaceDetail` through
/// `qualifyWorkspace`, which is `events.ts`'s `QualifiedWorkspace` exactly, and
/// `system` is the *primary's* `/v1/system` or null when it did not answer.
///
/// `daemons[].system` is still `unknown` — same fact, per machine, and the
/// place to fix it is `server/roster.ts`'s `DaemonDto`.
export interface WorldSnapshot extends Omit<WorldState, "workspaces" | "system"> {
  workspaces: QualifiedWorkspace[];
  system: SysDto | null;
}

/// The body of one git-operation route.
///
/// Every field is optional (`#[serde(default)]` on the Rust side, so a bare
/// `POST .../git/fetch` means "the obvious thing"), and the names are `GitOp`'s
/// own — `http_conn.rs`'s `FetchBody`/`PullBody`/… exist only to deserialise
/// into the variant. Deriving the body from the generated union rather than
/// writing it out again is what makes a renamed field a compile error here
/// instead of a silently ignored key on the daemon.
type OpBody<K extends GitOp["op"]> = Partial<Omit<Extract<GitOp, { op: K }>, "op">>;

export type FetchOptions = OpBody<"fetch">;
export type PullOptions = OpBody<"pull">;
export type PushOptions = OpBody<"push">;
export type StashOptions = OpBody<"stash">;

/// One page of history. `all` and `rev` are exclusive — the daemon 400s both.
export interface GitLogOptions {
  limit?: number;
  skip?: number;
  path?: string;
  rev?: string;
  all?: boolean;
}

// A daemon-scoped route with no workspace in it: the daemon has to be named
// some other way, and `?daemon=` is that way.
//
// Unused, and left in place: removing it is a change, and every method below
// spells its own `?daemon=` rather than calling it. See HANDOVER-api.md.
function on(daemon: string | null | undefined, path: string): string {
  const sep = path.indexOf("?") >= 0 ? "&" : "?";
  return daemon ? path + sep + "daemon=" + encodeURIComponent(daemon) : path;
}

// Guard for a workspace/pane id on its way into a path. An unqualified id is a
// programming mistake with no safe interpretation once there is more than one
// daemon, so it stops here with the name of the caller rather than reaching a
// machine that will happily answer for its own workspace 1.
//
// The `typeof` check outlives the types: a `string` here says nothing about
// whether it is *qualified*, and this file is called from code that is not all
// TypeScript yet.
function ws(id: string, what = "workspace id"): string {
  if (typeof id === "string" && /^[A-Za-z0-9._-]+:[0-9]+$/.test(id)) return id;
  throw new Error(`${what} ${JSON.stringify(id)} is not qualified — ids must be written <daemon>:<id>`);
}

/// The daemon's error envelope, `{"error":"..."}`, or "" when this is not one.
///
/// One function rather than the same `(data && data.error)` expression written
/// twice, because `data` is `unknown` here and narrowing it inline twice reads
/// worse than naming it once. Falsy in, falsy out — a reply carrying
/// `"error": null` is not an error, exactly as before.
function errorText(data: unknown): string {
  if (!data || typeof data !== "object" || !("error" in data)) return "";
  const e = (data as { error?: unknown }).error;
  return e ? String(e) : "";
}

async function req<T>(method: string, path: string, body?: unknown): Promise<T> {
  const opt: RequestInit = { method };
  if (body !== undefined) {
    opt.headers = { "content-type": "application/json" };
    opt.body = JSON.stringify(body);
  }
  const r = await fetch("/api" + path, opt);
  let data: unknown = null;
  try {
    data = await r.json();
  } catch { /* not JSON: an empty body, or something in front of the bridge */ }
  const failed = errorText(data);
  if (!r.ok || failed) {
    throw new Error(failed || `${method} ${path} -> ${r.status}`);
  }
  // The one cast in the file, and the whole trust boundary: `T` is the DTO the
  // route's `ApiReply` serialises, generated from the very Rust that sent it.
  return data as T;
}

export const api = {
  // Every configured daemon, without contacting any of them. Read first, so the
  // client knows how many event streams to open before it opens one — and so a
  // machine that is down still gets a marker instead of vanishing.
  daemons: (): Promise<DaemonsReply> => fetch("/api/daemons").then((r) => r.json() as Promise<DaemonsReply>),
  // Whole-world snapshot across every daemon: workspaces (with
  // agents/processes/changes/stage), the daemon roster, and the primary's system.
  state: (): Promise<WorldSnapshot> => fetch("/api/state").then((r) => r.json() as Promise<WorldSnapshot>),
  // Agent types are per daemon — the machine you are spawning on is the machine
  // whose config lists them.
  agentTypes: (daemon: string): Promise<string[]> =>
    req<string[]>("GET", `/agents?daemon=${encodeURIComponent(daemon)}`).catch(() => []),
  workspace: (id: string): Promise<WorkspaceDetail> => req<WorkspaceDetail>("GET", `/workspaces/${ws(id)}`),
  // `filter` decides the rows *and* their `changed` markers. Filtering the
  // answer here instead left directories marked for files this page had just
  // dropped, so the DOCS rail's `●` led to an empty listing — see `TreeFilter`
  // in the protocol crate.
  tree: (id: string, path = "", filter?: "docs"): Promise<TreeDto> =>
    req<TreeDto>(
      "GET",
      `/workspaces/${ws(id)}/tree?path=${encodeURIComponent(path)}${filter ? `&filter=${filter}` : ""}`,
    ),
  file: (id: string, path: string): Promise<FileDto> =>
    req<FileDto>("GET", `/workspaces/${ws(id)}/file?path=${encodeURIComponent(path)}`),
  diff: (id: string, path: string, staged?: boolean): Promise<DiffDto> =>
    req<DiffDto>("GET", `/workspaces/${ws(id)}/diff?path=${encodeURIComponent(path)}&kind=${staged ? "staged" : "unstaged"}`),
  // Whole-commit diff (git show) for a revision.
  show: (id: string, rev: string): Promise<DiffDto> =>
    req<DiffDto>("GET", `/workspaces/${ws(id)}/show?id=${encodeURIComponent(rev)}`),
  // Local git branches (current first) for the branch switcher.
  branches: (id: string): Promise<BranchesDto> => req<BranchesDto>("GET", `/workspaces/${ws(id)}/branches`),
  // Browse a host directory for the "create workspace in a folder" picker. The
  // directory is on the *daemon's* machine, so which daemon is part of the
  // question rather than a detail of how it is asked.
  // `path` null/"" defaults to that daemon user's home directory.
  browse: (daemon: string, path?: string | null): Promise<BrowseDto> =>
    req<BrowseDto>("GET", `/fs?path=${encodeURIComponent(path ?? "")}&daemon=${encodeURIComponent(daemon)}`),
  // A direct URL the browser can navigate to for a file download (the daemon
  // sets Content-Disposition, so this saves the file).
  downloadUrl: (id: string, path: string): string =>
    `/api/workspaces/${ws(id)}/download?path=${encodeURIComponent(path)}`,

  // actions
  // Create a folder under `path` (the picker's "New Folder"). Replies with the
  // listing of the new directory, so the caller navigates straight into it.
  mkdir: (daemon: string, path: string | null | undefined, name: string): Promise<BrowseDto> =>
    req<BrowseDto>("POST", `/fs/mkdir?daemon=${encodeURIComponent(daemon)}`, { path: path ?? "", name }),
  // Create a workspace, optionally in a chosen directory (the folder picker).
  newWorkspace: (daemon: string, name?: string | null, path?: string | null): Promise<CreatedReply> => {
    const b: { name?: string; path?: string } = {};
    if (name) b.name = name;
    if (path) b.path = path;
    return req<CreatedReply>("POST", `/workspaces?daemon=${encodeURIComponent(daemon)}`, b);
  },
  checkout: (id: string, branch: string, create = false): Promise<OkReply> =>
    req<OkReply>("POST", `/workspaces/${ws(id)}/checkout`, { branch, create }),
  // Upload a File/Blob to `path` (destination dir + filename) under the cwd.
  upload: async (id: string, path: string, file: Blob): Promise<OkReply> => {
    const r = await fetch(`/api/workspaces/${ws(id)}/upload?path=${encodeURIComponent(path)}`, {
      method: "POST",
      headers: { "content-type": file.type || "application/octet-stream" },
      body: file,
    });
    let data: unknown = null;
    try { data = await r.json(); } catch { /* see `req` */ }
    const failed = errorText(data);
    if (!r.ok || failed) {
      throw new Error(failed || `upload -> ${r.status}`);
    }
    return data as OkReply;
  },
  // Delete one file under the cwd. The inverse of `upload`, and named for the
  // route rather than for the verb: `discard` above is git's "put it back",
  // this one leaves nothing to put back.
  deleteFile: (id: string, path: string): Promise<OkReply> =>
    req<OkReply>("DELETE", `/workspaces/${ws(id)}/file?path=${encodeURIComponent(path)}`),
  killWorkspace: (id: string): Promise<OkReply> => req<OkReply>("DELETE", `/workspaces/${ws(id)}`),
  spawnAgent: (id: string, type: string): Promise<OkReply> =>
    req<OkReply>("POST", `/workspaces/${ws(id)}/agents`, { type }),
  newProcess: (id: string, name: string, command: string): Promise<OkReply> =>
    req<OkReply>("POST", `/workspaces/${ws(id)}/processes`, { name, command }),
  restartProcess: (id: string, pane: string): Promise<OkReply> =>
    req<OkReply>("POST", `/workspaces/${ws(id)}/processes/${ws(pane, "pane id")}/restart`),
  // Kill any pane — agent, process, editor, tree. `DELETE .../processes/{pane}`
  // is a legacy alias of this and reaches the same handler, but the `processes`
  // spelling read as a restriction and is the reason GUIs shipped without a way
  // to kill an *agent*. This client was one of them.
  killPane: (id: string, pane: string): Promise<OkReply> =>
    req<OkReply>("DELETE", `/workspaces/${ws(id)}/panes/${ws(pane, "pane id")}`),
  // killPane for a request made as the page is going away. `keepalive` lets it
  // outlive the document, which is the only way to reap a pane from a
  // `pagehide` handler — sendBeacon can't be used, it only sends POST. Nothing
  // reads the reply; there is no page left to read it.
  killPaneBeacon: (id: string, pane: string): Promise<Response | undefined> =>
    fetch(`/api/workspaces/${ws(id)}/panes/${ws(pane, "pane id")}`, { method: "DELETE", keepalive: true })
      .catch(() => undefined),
  // Dismiss a pane's pending bell without opening it. The daemon acknowledges a
  // pane when a client *looks* at it — an attach, or a `watch` — and a browser
  // reading the rails is not looking, so without this route an agent that rang
  // the bell stays `waiting` here forever no matter what you click.
  ackPane: (id: string, pane: string): Promise<OkReply> =>
    req<OkReply>("POST", `/workspaces/${ws(id)}/panes/${ws(pane, "pane id")}/ack`),
  // Type into a pane without attaching to it.
  //
  // **New here.** The route has existed on the daemon throughout and no browser
  // client ever called it — `web/README.md` listed it under "the nine it does
  // not reach", with the note that it was "the obvious next parity item: answer
  // this agent yes from a rail or from HOME's fleet". That is what it is for: a
  // blocked agent is the one thing on a workbench that stops everything, and
  // reaching it used to mean opening its pane first.
  //
  // Sending input also counts as *looking* at the pane, so the daemon clears
  // `unread` and the bell with it — answering a question does not leave the row
  // still lit.
  paneInput: (id: string, pane: string, text: string): Promise<OkReply> =>
    req<OkReply>("POST", `/workspaces/${ws(id)}/panes/${ws(pane, "pane id")}/input`, { text }),
  // Which agent account stops you first.
  //
  // **New here.** `web/README.md` listed this first under "the nine it does not
  // reach": "a terminal page so far — the daemon serves it for every client, and
  // this one has no surface for it yet". `UsagePage` is that surface.
  //
  // Per daemon, because an account limit is a fact about the machine the CLI
  // runs on: the same `claude` login on two boxes has two separate windows, and
  // merging them would report a ceiling neither of them has.
  usage: (daemon?: string): Promise<UsageDto> =>
    req<UsageDto>("GET", daemon ? `/usage?daemon=${encodeURIComponent(daemon)}` : "/usage"),
  // The bridge's own roster, not a daemon's. `POST` dials the socket before it
  // joins the list — an entry that has never answered is indistinguishable from
  // a machine that was fine and has just gone down, and the difference matters
  // most while somebody is typing a path. `DELETE` refuses an entry that came
  // from the environment, because removing one would come back on the next
  // restart.
  //
  // **Also new here.** The bridge has served both since the roster grew a
  // writer; no client had a caller, so the MACHINES group could show a list and
  // not change it.
  addDaemon: (body: { socket: string; name?: string }): Promise<DaemonDto> =>
    fetch("/api/daemons", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(body),
    }).then((r) => r.json() as Promise<DaemonDto>),
  removeDaemon: (key: string): Promise<{ removed: DaemonDto }> =>
    fetch(`/api/daemons/${encodeURIComponent(key)}`, { method: "DELETE" }).then(
      (r) => r.json() as Promise<{ removed: DaemonDto }>,
    ),
  // The daemon's sequence-numbered feed of agent transitions. It decides "an
  // agent finished" once, so every client that drains the feed agrees and none
  // of them re-derives it from snapshots. Answers `{head, items}`: pass the
  // highest seq you have processed as `since`, and advance to `head` afterwards
  // even when `items` is empty, so a fresh client does not replay history.
  //
  // The seq is a *daemon's* counter, so this feed is per daemon and so is the
  // cursor. One cursor across two machines silences whichever is behind.
  notifications: (daemon: string, since = 0): Promise<NotificationsDto> =>
    req<NotificationsDto>("GET", `/notifications?since=${since}&daemon=${encodeURIComponent(daemon)}`),
  stage: (id: string, path: string): Promise<OkReply> =>
    req<OkReply>("POST", `/workspaces/${ws(id)}/changes/stage`, { path }),
  unstage: (id: string, path: string): Promise<OkReply> =>
    req<OkReply>("POST", `/workspaces/${ws(id)}/changes/unstage`, { path }),
  commit: (id: string, message: string): Promise<OkReply> =>
    req<OkReply>("POST", `/workspaces/${ws(id)}/changes/commit`, { message }),
  commitAll: (id: string, message: string): Promise<OkReply> =>
    req<OkReply>("POST", `/workspaces/${ws(id)}/changes/commit-all`, { message }),
  discard: (id: string, path: string): Promise<OkReply> =>
    req<OkReply>("POST", `/workspaces/${ws(id)}/changes/discard`, { path }),
  // Git operations. These answer 200 when they finish inside the daemon's grace
  // window and 202 when they do not, so the result may be in the reply or may
  // have to be polled from `gitOp` — `runGitOp` below hides the difference.
  // Either way the reply is a `GitOpDto`: **check `ok`**, a rejected push is a
  // successful call reporting a failed operation.
  fetch: (id: string, opts: FetchOptions = {}): Promise<GitOpDto> =>
    req<GitOpDto>("POST", `/workspaces/${ws(id)}/git/fetch`, opts),
  pull: (id: string, opts: PullOptions = {}): Promise<GitOpDto> =>
    req<GitOpDto>("POST", `/workspaces/${ws(id)}/git/pull`, opts),
  push: (id: string, opts: PushOptions = {}): Promise<GitOpDto> =>
    req<GitOpDto>("POST", `/workspaces/${ws(id)}/git/push`, opts),
  gitOp: (id: string): Promise<GitOpDto> => req<GitOpDto>("GET", `/workspaces/${ws(id)}/git/op`),
  // One page of history. `all` walks every branch, tag and remote rather than
  // just HEAD — without it the "graph" only ever shows the branch you are on,
  // which is the whole feature missing. `rev` scopes it to one ref instead.
  // The daemon walks `--topo-order` either way, which is what lets the lanes be
  // assigned in one pass down the page (see graph.ts).
  gitLog: (id: string, { limit = 50, skip = 0, path, rev, all = false }: GitLogOptions = {}): Promise<LogDto> =>
    req<LogDto>("GET", `/workspaces/${ws(id)}/git/log?limit=${limit}&skip=${skip}` +
      (all ? "&all=1" : "") +
      (rev ? `&rev=${encodeURIComponent(rev)}` : "") +
      (path ? `&path=${encodeURIComponent(path)}` : "")),
  stashes: (id: string): Promise<StashDto[]> => req<StashDto[]>("GET", `/workspaces/${ws(id)}/git/stashes`),
  remotes: (id: string): Promise<RemoteDto[]> => req<RemoteDto[]>("GET", `/workspaces/${ws(id)}/git/remotes`),
  tags: (id: string): Promise<string[]> => req<string[]>("GET", `/workspaces/${ws(id)}/git/tags`),
  // Every checkout of this repository, **including which butai workspace is
  // already open on each** — so a row can offer "go there" rather than "open it
  // again", and so the checkout you are standing in is marked instead of being
  // offered a remove that only ever errors.
  worktrees: (id: string): Promise<WorktreeDto[]> =>
    req<WorktreeDto[]>("GET", `/workspaces/${ws(id)}/git/worktrees`),
  stash: (id: string, opts: StashOptions = {}): Promise<GitOpDto> =>
    req<GitOpDto>("POST", `/workspaces/${ws(id)}/git/stash`, opts),
  stashApply: (id: string, index = 0, pop = true): Promise<GitOpDto> =>
    req<GitOpDto>("POST", `/workspaces/${ws(id)}/git/stash/apply`, { index, pop }),
  stashDrop: (id: string, index = 0): Promise<GitOpDto> =>
    req<GitOpDto>("DELETE", `/workspaces/${ws(id)}/git/stash?index=${index}`),
  amend: (id: string, message?: string | null): Promise<GitOpDto> =>
    req<GitOpDto>("POST", `/workspaces/${ws(id)}/git/amend`, { message: message || null }),
  // `mode` is soft | mixed | hard. Hard discards the worktree, so every caller
  // of it confirms first — see git-menu.ts's `needsConfirm`.
  reset: (id: string, rev: string, mode: ResetMode = "mixed"): Promise<GitOpDto> =>
    req<GitOpDto>("POST", `/workspaces/${ws(id)}/git/reset`, { rev, mode }),
  revert: (id: string, rev: string): Promise<GitOpDto> =>
    req<GitOpDto>("POST", `/workspaces/${ws(id)}/git/revert`, { rev }),
  cherryPick: (id: string, rev: string): Promise<GitOpDto> =>
    req<GitOpDto>("POST", `/workspaces/${ws(id)}/git/cherry-pick`, { rev }),
  merge: (id: string, branch: string, noFf = false): Promise<GitOpDto> =>
    req<GitOpDto>("POST", `/workspaces/${ws(id)}/git/merge`, { branch, no_ff: noFf }),
  rebase: (id: string, onto: string): Promise<GitOpDto> =>
    req<GitOpDto>("POST", `/workspaces/${ws(id)}/git/rebase`, { onto }),
  tag: (id: string, name: string, rev?: string | null, message?: string | null): Promise<GitOpDto> =>
    req<GitOpDto>("POST", `/workspaces/${ws(id)}/git/tag`, { name, rev: rev || null, message: message || null }),
  tagDelete: (id: string, name: string): Promise<GitOpDto> =>
    req<GitOpDto>("DELETE", `/workspaces/${ws(id)}/git/tag?name=${encodeURIComponent(name)}`),
  // The one route in the API that takes a URL. The daemon validates it against
  // an allowlist of transports before it reaches git (`git_op::valid_remote_url`
  // — `ext::` transport helpers are refused outright), so a rejected URL comes
  // back as a 400 with the reason and is shown as-is rather than swallowed.
  remoteAdd: (id: string, name: string, url: string): Promise<GitOpDto> =>
    req<GitOpDto>("POST", `/workspaces/${ws(id)}/git/remote`, { name, url }),
  remoteRemove: (id: string, name: string): Promise<GitOpDto> =>
    req<GitOpDto>("DELETE", `/workspaces/${ws(id)}/git/remote?name=${encodeURIComponent(name)}`),
  worktreeAdd: (id: string, path: string, branch?: string | null, newBranch = false): Promise<GitOpDto> =>
    req<GitOpDto>("POST", `/workspaces/${ws(id)}/git/worktree`, { path, branch: branch || null, new_branch: newBranch }),
  worktreeRemove: (id: string, path: string, force = false): Promise<GitOpDto> =>
    req<GitOpDto>("DELETE", `/workspaces/${ws(id)}/git/worktree?path=${encodeURIComponent(path)}${force ? "&force=1" : ""}`),
  worktreePrune: (id: string): Promise<GitOpDto> =>
    req<GitOpDto>("POST", `/workspaces/${ws(id)}/git/worktree/prune`, {}),
  branchCreate: (id: string, name: string, from?: string | null): Promise<OkReply> =>
    req<OkReply>("POST", `/workspaces/${ws(id)}/git/branch`, { name, from: from || null }),
  branchDelete: (id: string, name: string, force = false): Promise<OkReply> =>
    req<OkReply>("DELETE", `/workspaces/${ws(id)}/git/branch?name=${encodeURIComponent(name)}${force ? "&force=1" : ""}`),
  branchRename: (id: string, from: string | null | undefined, to: string): Promise<OkReply> =>
    req<OkReply>("POST", `/workspaces/${ws(id)}/git/branch/rename`, { from: from || null, to }),
  // Settle one conflicted file: "ours", "theirs", or "resolved" once it has
  // been edited by hand. Synchronous — index work, not a runner operation.
  resolve: (id: string, path: string, take: ResolveSide): Promise<OkReply> =>
    req<OkReply>("POST", `/workspaces/${ws(id)}/git/resolve`, { path, take }),
  // Drive whatever merge/rebase/cherry-pick/revert is in progress.
  sequence: (id: string, action: SequenceAction): Promise<GitOpDto> =>
    req<GitOpDto>("POST", `/workspaces/${ws(id)}/git/sequence`, { action }),
  conflict: (id: string, path: string): Promise<ConflictDto> =>
    req<ConflictDto>("GET", `/workspaces/${ws(id)}/git/conflict?path=${encodeURIComponent(path)}`),
  cancelGitOp: (id: string): Promise<GitOpDto> => req<GitOpDto>("DELETE", `/workspaces/${ws(id)}/git/op`),
};

/// How `runGitOp` waits.
export interface RunGitOpOptions {
  timeoutMs?: number;
  /// The push channel: hands back a promise that settles with this workspace's
  /// `git_op` event. `null` when the stream is down or the client has none.
  watch?: ((id: string) => Promise<GitOpDto>) | null;
}

// Run a git operation to completion regardless of which way the daemon answered.
// `start` is one of api.fetch/pull/push. A finished operation is returned as-is;
// one still running is waited on. The returned object carries `ok` and
// `summary`: a rejected push is a *successful* call reporting a failed
// operation, so callers must check `ok` rather than relying on a thrown error.
//
// `watch(id)` is the push channel: a promise that settles with the `git_op`
// event the daemon sends when this workspace's operation stops. That is the
// whole reason `ApiEvent::GitOp` exists, and the 300ms poll below is what it
// was added to replace — kept, because it is also what runs when the stream is
// down. Whichever answers first wins; a `git push` over a slow link is the case
// that makes the difference visible, since the daemon has git's progress line
// and the poll has only a clock.
export async function runGitOp(
  start: () => Promise<GitOpDto>,
  id: string,
  { timeoutMs = 120000, watch = null }: RunGitOpOptions = {},
): Promise<GitOpDto> {
  const first = await start();
  if (first && first.running === false) return first;
  // Registered *after* the start replied: from here on, a `git_op` for this
  // workspace is this operation, because the daemon runs one at a time per
  // workspace. The sliver between the reply and this line is covered by the
  // backstop below, which is also the whole mechanism when there is no stream.
  const pushed = watch ? watch(id) : null;
  const backstopMs = pushed ? 3000 : 300;
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    const settled = await Promise.race<GitOpDto | null>([
      pushed || new Promise<GitOpDto | null>(() => {}),
      new Promise<null>((r) => setTimeout(() => r(null), backstopMs)),
    ]);
    if (settled) return settled;
    const state = await api.gitOp(id).catch(() => null);
    if (state && !state.running) return state;
  }
  throw new Error("git operation did not finish in time");
}
