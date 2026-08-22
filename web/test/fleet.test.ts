// HOME's row model: every agent on every machine, in one list.
//
// Ported from `check.py`'s `check_fleet` / `FLEET_JS`. The Python copied
// `fleet.js` to a temp `.mjs`, wrote a probe that imported it and printed JSON,
// ran node, and asserted on the parsed result in Python. All of that existed to
// get a value out of a JavaScript module and into a test — which is what an
// import is. So the probe is gone and the fixture and its expectations are what
// survives.
//
// The expectations are lifted digit for digit. They are the accumulated
// knowledge of this page and re-deriving them would only re-derive today's
// behaviour, which is the one thing a regression test must not do.
//
// **Three machines whose ids collide on purpose.** Every one of them has a
// workspace 1 and a pane 4 and a pane 5. A fleet built from one machine's
// workspaces, or grouped by a bare id, renders a *plausible* page — the wrong
// machine's agent under the right machine's header — and nothing anywhere
// reports a problem.

import { describe, expect, test } from "bun:test";
import {
  allAgentRows,
  homeRows,
  homeTray,
  machineIsDown,
  machineRows,
  HomeRowKind,
  type AgentRow,
  type DaemonEntry,
  type MachineRow,
  type Workspace,
} from "../src/logic/fleet.ts";

type AgentSeed = { pane: number; title: string; state: string; exited?: number; unread?: boolean };

const agent = (pane: number, title: string, state: string, extra: Partial<AgentSeed> = {}): AgentSeed => ({
  pane,
  title,
  state,
  ...extra,
});

const ws = (daemon: string, id: number, name: string, agents: AgentSeed[]): Workspace =>
  ({
    id: `${daemon}:${id}`,
    daemon,
    name,
    agents: agents.map((a) => ({ ...a, pane: `${daemon}:${a.pane}` })),
    processes: [{ pane: `${daemon}:1`, name: "shell" }],
  }) as unknown as Workspace;

const sys = (cpu: number) => ({
  cpu_pct: cpu,
  ram_used_gb: 4,
  ram_total_gb: 16,
  gpus: [],
  containers: [],
  stacks: [],
});

const workspaces: Workspace[] = [
  ws("a", 1, "alpha", [agent(4, "ringer", "waiting"), agent(5, "pinger", "idle")]),
  ws("a", 2, "alto", [agent(9, "pinger", "idle")]),
  ws("b", 1, "bravo", [agent(4, "ringer", "waiting"), agent(5, "quitter", "exited", { exited: 3 })]),
  ws("c", 1, "chi", [agent(4, "pinger", "idle")]),
];

const roster = [
  { key: "a", label: "a", primary: true, error: null, system: sys(10) },
  { key: "b", label: "b", primary: false, error: null, system: sys(20) },
  { key: "c", label: "c", primary: false, error: null, system: sys(30) },
  { key: "ghost", label: "ghost", primary: false, error: "No such file or directory", system: null },
] as unknown as DaemonEntry[];

const all: AgentRow[] = allAgentRows(workspaces, roster);
const machines: MachineRow[] = machineRows(roster, all);
const rows = homeRows(all, machines);

describe("the fleet list", () => {
  test("fleet/rows — machine by machine in roster order, every row saying which", () => {
    expect(all.map((r) => [r.pane, r.ws, r.daemon, r.host, r.workspace, r.agent.title])).toEqual([
      ["a:4", "a:1", "a", "a", "alpha", "ringer"],
      ["a:5", "a:1", "a", "a", "alpha", "pinger"],
      ["a:9", "a:2", "a", "a", "alto", "pinger"],
      ["b:4", "b:1", "b", "b", "bravo", "ringer"],
      ["b:5", "b:1", "b", "b", "bravo", "quitter"],
      ["c:4", "c:1", "c", "c", "chi", "pinger"],
    ]);
  });

  test("fleet/qualified — no row carries a bare id", () => {
    // The whole point of the colliding fixture: a bare `4` would match three
    // machines' panes and attach to whichever the client asked first.
    for (const r of all) {
      expect(String(r.pane)).toContain(":");
      expect(String(r.ws)).toContain(":");
    }
  });

  test("fleet/roster-order — the grouping is the roster's, not the workspace list's", () => {
    const reordered = allAgentRows(workspaces, [roster[2]!, roster[0]!, roster[1]!]);
    expect(reordered.map((r) => r.daemon)).toEqual(["c", "a", "a", "a", "b", "b"]);
  });

  test("fleet/no-machine-is-dropped — a workspace whose daemon is not on the roster still appears", () => {
    // Losing it would be a machine's agents vanishing from the page that exists
    // to show every machine's agents.
    const stranger = allAgentRows([...workspaces, ws("z", 1, "zulu", [agent(4, "lost", "idle")])], roster);
    expect(stranger.map((r) => r.daemon)).toContain("z");
  });

  test("fleet/one-machine-no-badge — nothing to qualify against, so no badge", () => {
    expect(allAgentRows([workspaces[0]!], [roster[0]!]).map((r) => r.host)).toEqual([null, null]);
  });
});

describe("the grouped page", () => {
  test("fleet/home-rows — machine, then project, then that project's agents", () => {
    const shape = rows.map((r) =>
      r.kind === HomeRowKind.Machine
        ? `machine:${r.label}:${r.agents}`
        : r.kind === HomeRowKind.Space
          ? `space:${r.ws}`
          : `agent:${r.sel}:${r.row.pane}`,
    );
    expect(shape).toEqual([
      "machine:a:3", "space:a:1", "agent:0:a:4", "agent:1:a:5",
      "space:a:2", "agent:2:a:9",
      "machine:b:2", "space:b:1", "agent:3:b:4", "agent:4:b:5",
      "machine:c:1", "space:c:1", "agent:5:c:4",
    ]);
  });

  test("fleet/sel-is-contiguous — `sel` counts agents only, which is what the cursor walks", () => {
    const sels = rows.filter((r) => r.kind === HomeRowKind.Agent).map((r) => r.sel);
    expect(sels).toEqual(sels.map((_, i) => i));
  });

  test("fleet/spaces-are-ids — two projects sharing a name are two headers", () => {
    // The id is what is unique; the name is only what is printed.
    const dup = homeRows(
      allAgentRows(
        [ws("a", 1, "dup", [agent(4, "x", "idle")]), ws("a", 2, "dup", [agent(5, "y", "idle")])],
        [roster[0]!],
      ),
      [],
    );
    expect(dup.filter((r) => r.kind === HomeRowKind.Space).map((r) => r.ws)).toEqual(["a:1", "a:2"]);
  });
});

describe("the tray", () => {
  test("fleet/tray — the blocked agents, carrying the original's index", () => {
    expect(homeTray(all).map((t) => [t.sel, t.row.pane])).toEqual([
      [0, "a:4"],
      [3, "b:4"],
    ]);
  });

  test("fleet/tray-copies — the tray copies upward and leaves the fleet's order alone", () => {
    // Sorting the fleet itself by urgency was measured and rejected (~174 row
    // moves per ten sampler ticks at 24 agents), which is why the tray copies.
    expect(all.map((r) => r.pane)).toEqual(["a:4", "a:5", "a:9", "b:4", "b:5", "c:4"]);
  });

  test("fleet/tray-ranks-unread — blocked, then an unread crash, then unread turns", () => {
    // The tray draws four rows and does not scroll, so the ranking is what
    // decides whether a blocked agent is visible at all when three land at once.
    const ranked = homeTray(
      allAgentRows(
        [
          ws("a", 1, "alpha", [
            agent(1, "landed", "finished", { unread: true }),
            agent(2, "crashed", "exited", { exited: 2, unread: true }),
            agent(3, "read-turn", "finished"),
            agent(4, "read-crash", "exited", { exited: 2 }),
            agent(5, "blocked", "waiting"),
          ]),
        ],
        [roster[0]!],
      ),
    ).map((t) => t.row.agent.title);
    expect(ranked).toEqual(["blocked", "crashed", "landed"]);
  });
});

describe("the machines column", () => {
  test("fleet/machines — one block per configured daemon", () => {
    expect(machines.map((m) => [m.daemon, m.label, m.agents, m.sys ? m.sys.cpu_pct : null, m.error, machineIsDown(m)])).toEqual([
      ["a", "a", 3, 10, null, false],
      ["b", "b", 2, 20, null, false],
      ["c", "c", 1, 30, null, false],
      ["ghost", "ghost", 0, null, "No such file or directory", true],
    ]);
  });

  test("fleet/machines-not-merged — telemetry per machine, never pooled", () => {
    const cpus = machines.filter((m) => m.sys).map((m) => m.sys!.cpu_pct);
    expect(new Set(cpus).size).toBe(cpus.length);
    expect(cpus.length).toBeGreaterThanOrEqual(3);
  });

  test("fleet/machines-keep-the-down-one — an unreachable machine is a marker, not an omission", () => {
    expect(machines.some((m) => machineIsDown(m))).toBe(true);
  });
});

test("fleet/empty — nothing in, nothing out, and no throw", () => {
  expect([
    homeRows([], machines).length,
    homeTray([]).length,
    allAgentRows(null, null).length,
    machineRows(null, null).length,
  ]).toEqual([0, 0, 0, 0]);
});
