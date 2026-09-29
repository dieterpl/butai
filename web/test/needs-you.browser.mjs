// Run: bun test/needs-you.browser.mjs. Uses a fixture bridge and isolated Vite server.
import { chromium } from "playwright";
import { spawn } from "node:child_process";
import { mkdir } from "node:fs/promises";
import assert from "node:assert/strict";
const server = spawn("bun", ["run", "dev", "--host", "127.0.0.1", "--port", "5188", "--strictPort"], { stdio: "ignore" });
let browser;
const errors = [], writes = [], messages = [];
const system = { cpu_pct: 24, cpu_temp: 50, cpu_hist: [10, 20, 24], cpu_model: "Test CPU", cpu_cores: 4, cpu_threads: 8,
  ram_used_gb: 8, ram_total_gb: 32, ram_hist: [20, 22, 25], swap_used_gb: 0, swap_total_gb: 0,
  gpus: [], net: [{ name: "eth0", kind: "wired", carrier: true, speed_mbps: 1000, rx_bps: 8000, tx_bps: 4000, rx_hist: [0, 8000], tx_hist: [0, 4000] }],
  disks: [], containers: [], stacks: [] };
const daemon = { key: "local", label: "local", socket: "/test.sock", primary: true, source: "test", error: null, system };
const agent = (pane, title) => ({ pane, title, state: "idle", exited: null, question: false, started_ms: 0, unread: false });
const changes = { branch: "main", ahead: 0, behind: 0, upstream: null, state: "clean", conflicted: [], unstaged: [{ path: "src/main.rs", code: "M", added: 2, deleted: 1 }], staged: [], recent_commits: [] };
const workspace = (id, name, agents) => ({ id, daemon: "local", name, cwd: `/test/${name}`, agents, processes: [], changes, stage: agents[0]?.pane ?? null });
const spaces = [workspace("local:1", "alpha", [agent("local:11", "codex"), agent("local:12", "claude")]), workspace("local:2", "beta", [agent("local:21", "gemini")])];
try {
  for (let i = 0; i < 100; i++) {
    try { if ((await fetch("http://127.0.0.1:5188")).ok) break; } catch {}
    await new Promise(r => setTimeout(r, 100));
  }
  browser = await chromium.launch();
  const page = await browser.newPage({ viewport: { width: 1280, height: 800 } });
  page.setDefaultTimeout(5000);
  page.on("pageerror", e => errors.push(e.message));
  await page.routeWebSocket("**/ws?*", ws => ws.onMessage(m => messages.push(JSON.parse(m))));
  await page.route("**/api/**", async route => {
    const req = route.request(), path = new URL(req.url()).pathname;
    if (req.method() !== "GET") writes.push({ path, method: req.method(), body: req.postDataJSON() });
    let body = { ok: true };
    if (path === "/api/state") body = { daemons: [daemon], workspaces: spaces, system };
    else if (path === "/api/daemons") body = { daemons: [daemon] };
    else if (path === "/api/agents") body = ["codex", "claude", "gemini"];
    else if (path === "/api/events") { await route.fulfill({ status: 200, contentType: "text/event-stream", body: ": fixture\n\n" }); return; }
    else if (path.endsWith("/branches")) body = { current: "main", branches: ["main"], entries: [] };
    else if (/\/git\/(tags|stashes|remotes|worktrees)$/.test(path)) body = [];
    else if (path.endsWith("/git/log")) body = { commits: [], more: false };
    else if (path.endsWith("/tree")) body = { path: "", entries: [] };
    else if (path.endsWith("/diff")) body = { path: "src/main.rs", patch: "--- a/src/main.rs\n+++ b/src/main.rs\n@@ -1 +1 @@\n-old\n+new" };
    else if (path === "/api/workspaces" && req.method() === "POST") body = { id: 3 };
    else if (/\/workspaces\/local:\d+$/.test(path)) body = spaces.find(w => path.endsWith(w.id)) ?? spaces[0];
    await route.fulfill({ status: 200, contentType: "application/json", body: JSON.stringify(body) });
  });
  await page.goto("http://127.0.0.1:5188");
  await page.getByRole("button", { name: "Switch view" }).waitFor();
  await page.keyboard.press("Alt+0");
  const tray = page.getByRole("listbox", { name: "needs you", exact: true });
  const fleet = page.getByRole("listbox", { name: "fleet", exact: true });
  await tray.getByText("nothing needs you").waitFor();
  const fleetTop = await fleet.evaluate(el => el.getBoundingClientRect().top);

  spaces[0].agents[1].state = "waiting";
  await page.reload();
  await page.getByRole("button", { name: "Switch view" }).waitFor();
  await page.keyboard.press("Alt+0");
  const asking = tray.getByRole("option").filter({ hasText: "claude" });
  await asking.waitFor();
  assert.equal(await fleet.evaluate(el => el.getBoundingClientRect().top), fleetTop, "attention moved the fleet");
  await asking.click();
  await page.waitForFunction(() => document.activeElement?.closest('[data-slot="stage"]'));
  await page.waitForFunction(() => document.querySelector('[aria-label="needs you"] [aria-selected="true"]')?.textContent.includes("claude"));
  await page.locator("main").getByText("claude · alpha", { exact: true }).waitFor();
  assert.equal(await fleet.getByRole("option").filter({ hasText: "claude" }).getAttribute("aria-selected"), "true");
  // Closing a different fleet chat must not select it or close the preview.
  await fleet.getByRole("button", { name: "Close gemini in beta", exact: true }).click();
  await page.getByRole("dialog", { name: "End gemini?" }).waitFor();
  await page.getByRole("button", { name: "Cancel", exact: true }).click();
  assert.equal(writes.filter(w => w.method === "DELETE").length, 0);
  await page.locator("main").getByText("claude · alpha", { exact: true }).waitFor();
  await fleet.getByRole("button", { name: "Close gemini in beta", exact: true }).click();
  await page.getByRole("button", { name: "Yes, do it", exact: true }).click();
  await page.waitForResponse(r => r.request().method() === "GET" && new URL(r.url()).pathname === "/api/state");
  assert.deepEqual(writes.filter(w => w.method === "DELETE").map(w => decodeURIComponent(w.path)), ["/api/workspaces/local:2/panes/local:21"]);
  await page.getByTitle("Hide chats in alpha", { exact: true }).click();
  assert.equal(await asking.count(), 1, "folding hid the attention copy");
  await asking.click();
  await page.getByTitle("Hide chats in alpha", { exact: true }).waitFor();
  await page.waitForFunction(() => document.activeElement?.closest('[data-slot="stage"]'));
  await page.getByTitle("Hide chats in alpha", { exact: true }).click();
  await asking.getByRole("button", { name: "Close claude in alpha", exact: true }).click();
  await page.getByRole("dialog", { name: "End claude?" }).waitFor();
  await page.getByRole("button", { name: "Yes, do it", exact: true }).click();
  await page.waitForResponse(r => r.request().method() === "GET" && new URL(r.url()).pathname === "/api/state");
  assert.deepEqual(writes.filter(w => w.method === "DELETE").map(w => decodeURIComponent(w.path)), ["/api/workspaces/local:2/panes/local:21", "/api/workspaces/local:1/panes/local:12"]);
  await page.getByTitle("Show chats in alpha", { exact: true }).click();
  await mkdir("/var/tmp/butai-web-parity", { recursive: true });
  await page.screenshot({ path: "/var/tmp/butai-web-parity/needs-you.png" });
  await page.setViewportSize({ width: 320, height: 640 });
  assert.ok(await tray.isVisible(), "narrow BOOTH hides NEEDS YOU");
  assert.ok(await fleet.isVisible(), "narrow BOOTH hides the fleet");
  assert.ok(await page.getByRole("button", { name: "Switch view" }).isVisible());
  assert.deepEqual(errors, []);
  console.log("PASS: NEEDS YOU empty state, stable layout, preview selection, folded visibility and chat close buttons");
} finally { await browser?.close(); server.kill(); }
