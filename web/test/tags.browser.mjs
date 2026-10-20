// Run: bun test/tags.browser.mjs. Uses a fixture bridge and isolated Vite server.
//
// BOOTH's tags: a role and labels show beside a chat, a right-click opens the
// menu at the pointer, picking writes through the bridge and the row follows,
// and a machine's `+` asks where and opens a workspace there.
import { chromium } from "playwright";
import { spawn } from "node:child_process";
import { mkdir } from "node:fs/promises";
import assert from "node:assert/strict";
const server = spawn("bun", ["run", "dev", "--host", "127.0.0.1", "--port", "5189", "--strictPort"], { stdio: "ignore" });
let browser;
const errors = [], writes = [];
const system = { cpu_pct: 24, cpu_temp: 50, cpu_hist: [10, 20, 24], cpu_model: "Test CPU", cpu_cores: 4, cpu_threads: 8,
  ram_used_gb: 8, ram_total_gb: 32, ram_hist: [20, 22, 25], swap_used_gb: 0, swap_total_gb: 0,
  gpus: [], net: [{ name: "eth0", kind: "wired", carrier: true, speed_mbps: 1000, rx_bps: 8000, tx_bps: 4000, rx_hist: [0, 8000], tx_hist: [0, 4000] }],
  disks: [], containers: [], stacks: [] };
const daemon = { key: "local", label: "local", socket: "/test.sock", primary: true, source: "test", error: null, system };
const agent = (pane, title, extra = {}) => ({ pane, title, state: "idle", exited: null, question: false, started_ms: 0, unread: false, role: null, labels: [], ...extra });
const changes = { branch: "main", ahead: 0, behind: 0, upstream: null, state: "clean", conflicted: [], unstaged: [], staged: [], recent_commits: [] };
const workspace = (id, name, agents) => ({ id, daemon: "local", name, cwd: `/test/${name}`, agents, processes: [], changes, stage: agents[0]?.pane ?? null });
const spaces = [
  workspace("local:1", "alpha", [agent("local:11", "codex", { role: "worker", labels: ["bug"] }), agent("local:12", "claude")]),
  workspace("local:2", "beta", [agent("local:21", "gemini", { state: "waiting", role: "lead" })]),
];
try {
  for (let i = 0; i < 100; i++) {
    try { if ((await fetch("http://127.0.0.1:5189")).ok) break; } catch {}
    await new Promise(r => setTimeout(r, 100));
  }
  browser = await chromium.launch();
  const page = await browser.newPage({ viewport: { width: 1280, height: 800 } });
  page.setDefaultTimeout(5000);
  page.on("pageerror", e => errors.push(e.message));
  await page.routeWebSocket("**/ws?*", ws => ws.onMessage(() => {}));
  await page.route("**/api/**", async route => {
    const req = route.request(), path = decodeURIComponent(new URL(req.url()).pathname);
    if (req.method() !== "GET") writes.push({ path, method: req.method(), body: req.postDataJSON() });
    let body = { ok: true };
    const tags = path.match(/^\/api\/workspaces\/(local:\d+)\/panes\/(local:\d+)\/tags$/);
    if (tags && req.method() === "PUT") {
      const a = spaces.find(w => w.id === tags[1]).agents.find(x => x.pane === tags[2]);
      a.role = req.postDataJSON().role ?? null;
      a.labels = req.postDataJSON().labels ?? [];
    } else if (path === "/api/state") body = { daemons: [daemon], workspaces: spaces, system };
    else if (path === "/api/daemons") body = { daemons: [daemon] };
    else if (path === "/api/agents") body = ["codex", "claude", "gemini"];
    else if (path === "/api/events") { await route.fulfill({ status: 200, contentType: "text/event-stream", body: ": fixture\n\n" }); return; }
    else if (path.endsWith("/branches")) body = { current: "main", branches: ["main"], entries: [] };
    else if (/\/git\/(tags|stashes|remotes|worktrees)$/.test(path)) body = [];
    else if (path.endsWith("/git/log")) body = { commits: [], more: false };
    else if (path.endsWith("/tree")) body = { path: "", entries: [] };
    else if (path === "/api/workspaces" && req.method() === "POST") body = { id: 3 };
    else if (/\/workspaces\/local:\d+$/.test(path)) body = spaces.find(w => path.endsWith(w.id)) ?? spaces[0];
    await route.fulfill({ status: 200, contentType: "application/json", body: JSON.stringify(body) });
  });
  await page.goto("http://127.0.0.1:5189");
  await page.getByRole("button", { name: "Switch view" }).waitFor();
  await page.keyboard.press("Alt+0");
  const fleet = page.getByRole("listbox", { name: "fleet", exact: true });
  const tray = page.getByRole("listbox", { name: "needs you", exact: true });
  // CSS locators, not roles: an open menu is modal and Radix hides the page
  // behind it from the accessibility tree, which role queries respect.
  const chat = name => page.locator('[aria-label="fleet"] [role="option"]').filter({ hasText: name });

  // Roles and labels are drawn beside the chat, and in the needs-you copy.
  await chat("codex").getByText("worker", { exact: true }).waitFor();
  await chat("codex").getByText("#bug", { exact: true }).waitFor();
  await tray.getByRole("option").filter({ hasText: "gemini" }).getByText("lead", { exact: true }).waitFor();
  assert.equal(await chat("claude").getByText(/^(lead|worker|loop|watch)$/).count(), 0, "an untagged chat draws nothing");

  // Right-click opens the menu at the pointer, not at the page's corner.
  const box = await chat("claude").boundingBox();
  const at = { x: box.x + 60, y: box.y + box.height / 2 };
  await page.mouse.click(at.x, at.y, { button: "right" });
  const menu = page.getByRole("menu", { name: "Tag claude" });
  await menu.waitFor();
  const m = await menu.boundingBox();
  assert.ok(Math.abs(m.x - at.x) < 40 && Math.abs(m.y - at.y) < 60, `menu opened at ${m.x},${m.y}, pointer at ${at.x},${at.y}`);
  for (const r of ["lead", "worker", "loop", "watch"]) await menu.getByRole("menuitemcheckbox", { name: new RegExp(`^\\S?\\s*${r}\\b`) }).waitFor();

  // Picking a role writes the whole set, keeps the menu open, and the row follows.
  await menu.getByRole("menuitemcheckbox", { name: /loop/ }).click();
  await chat("claude").getByText("loop", { exact: true }).waitFor();
  assert.deepEqual(writes.at(-1), { path: "/api/workspaces/local:1/panes/local:12/tags", method: "PUT", body: { role: "loop", labels: [] } });
  assert.ok(await menu.isVisible(), "the menu stays open for a second pick");
  await menu.getByRole("menuitemcheckbox", { name: /#\s*review/ }).click();
  await chat("claude").getByText("#review", { exact: true }).waitFor();
  assert.deepEqual(writes.at(-1).body, { role: "loop", labels: ["review"] });
  // Picking the current role clears it, keeping the label.
  await menu.getByRole("menuitemcheckbox", { name: /loop/ }).click();
  await page.waitForFunction(() => ![...document.querySelectorAll('[aria-label="fleet"] [role="option"]')].some(o => o.textContent.includes("claude") && o.textContent.includes("loop")));
  assert.deepEqual(writes.at(-1).body, { role: null, labels: ["review"] });
  await mkdir("/var/tmp/butai-web-parity", { recursive: true });
  await page.screenshot({ path: "/var/tmp/butai-web-parity/tag-menu.png" });
  await page.keyboard.press("Escape");
  await menu.waitFor({ state: "hidden" });

  // A label made once is offered on every other chat.
  await page.mouse.click(at.x, at.y, { button: "right" });
  await menu.getByRole("menuitem", { name: /new label/ }).click();
  await page.getByRole("textbox", { name: "Label", exact: true }).fill("Infra");
  await page.getByRole("button", { name: "Add", exact: true }).click();
  await chat("claude").getByText("#infra", { exact: true }).waitFor();
  assert.deepEqual(writes.at(-1).body, { role: null, labels: ["review", "infra"] });
  const other = await chat("gemini").boundingBox();
  await page.mouse.click(other.x + 60, other.y + other.height / 2, { button: "right" });
  await page.getByRole("menu", { name: "Tag gemini" }).getByRole("menuitemcheckbox", { name: /#\s*infra/ }).waitFor();
  await page.keyboard.press("Escape");

  // The keyboard route: the `⋯` button opens the same menu under itself.
  await chat("codex").locator('[aria-label="Tag codex"]').focus();
  await page.keyboard.press("Enter");
  await page.getByRole("menu", { name: "Tag codex" }).waitFor();
  await page.keyboard.press("Escape");

  // `+` on a machine asks where, on that machine, and stays on the page.
  const before = writes.length;
  await page.getByRole("button", { name: "New workspace on local" }).click();
  await page.getByRole("textbox", { name: "Directory", exact: true }).fill("/srv/new-project");
  await page.getByRole("button", { name: "Open", exact: true }).click();
  await page.waitForResponse(r => r.request().method() === "GET" && new URL(r.url()).pathname === "/api/state");
  const made = writes.slice(before).find(w => w.method === "POST" && w.path === "/api/workspaces");
  assert.ok(made, "no workspace was created");
  assert.equal(made.body.path, "/srv/new-project");
  assert.ok(await fleet.isVisible(), "creating a workspace left BOOTH");

  assert.deepEqual(errors, []);
  console.log("PASS: roles and labels drawn, menu at the pointer, picks written and followed, new label offered fleet-wide, keyboard route, + on a machine");
} finally { await browser?.close(); server.kill(); }
