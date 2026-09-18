import { expect, test } from "bun:test";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

const executable = Bun.env.BUTAI_WEB_EXE;
test.skipIf(process.platform !== "win32" || !executable)("standalone Windows launcher serves assets, REST, SSE and framed WebSocket", async () => {
  const root = mkdtempSync(join(tmpdir(), "butai-web-"));
  const home = join(root, "state");
  const port = 18743;
  const origin = `http://127.0.0.1:${port}`;
  const child = Bun.spawn([executable!], {
    cwd: root,
    env: { ...process.env, BUTAI_HOME: home, LOCALAPPDATA: root, BUTAI_WEB_NO_OPEN: "1", PORT: String(port) },
    stdout: "pipe", stderr: "pipe",
  });
  try {
    let ready = false;
    for (let i = 0; i < 200; i++) {
      try {
        const response = await fetch(`${origin}/api/state`);
        const state = await response.json();
        ready = response.ok && Array.isArray(state.daemons) && state.daemons.length === 1 && state.daemons[0].error === null;
      } catch { /* starting */ }
      if (ready) break;
      if (child.exitCode !== null) throw new Error(await new Response(child.stderr).text());
      await Bun.sleep(100);
    }
    expect(ready).toBe(true);
    const html = await (await fetch(origin)).text();
    expect(html).toContain('<div id="root">');
    const asset = html.match(/src="([^"]+\.js)"/)?.[1];
    expect(asset).toBeDefined();
    expect((await fetch(new URL(asset!, `${origin}/`))).status).toBe(200);
    expect((await fetch(`${origin}/api/state`, { headers: { Origin: "https://example.com" } })).status).toBe(403);
    expect((await (await fetch(`${origin}/api/state`)).json()).daemons).toBeDefined();
    const abort = new AbortController();
    const events = await fetch(`${origin}/api/events`, { signal: abort.signal });
    expect(events.headers.get("Content-Type")).toContain("text/event-stream");
    const first = await events.body!.getReader().read();
    expect(new TextDecoder().decode(first.value)).toContain("retry:");
    abort.abort();
    await new Promise<void>((resolve, reject) => {
      const ws = new WebSocket(`ws://127.0.0.1:${port}/ws`);
      const timeout = setTimeout(() => { ws.close(); reject(new Error("WebSocket hello timed out")); }, 10000);
      ws.onopen = () => ws.send(JSON.stringify({ hello: { proto_version: 1, encoding: "json", cols: 80, rows: 24, target: "control", cwd: root } }));
      ws.onmessage = (event) => {
        try {
          expect(JSON.parse(String(event.data)).hello.server_version).toBeDefined();
          clearTimeout(timeout); ws.close(); resolve();
        } catch (error) { clearTimeout(timeout); ws.close(); reject(error); }
      };
      ws.onerror = () => { clearTimeout(timeout); ws.close(); reject(new Error("WebSocket failed")); };
    });
  } finally {
    child.kill();
    await child.exited;
    // Stop only the daemon created in this test's isolated state directory.
    const binaries = new Bun.Glob("butai/web-runtime/*/butai.exe");
    for await (const binary of binaries.scan({ cwd: root, absolute: true })) {
      await Bun.spawn([binary, "--socket", join(home, "butai.sock"), "kill-server", "--clear"], {
        env: { ...process.env, BUTAI_HOME: home }, stdout: "ignore", stderr: "ignore",
      }).exited;
    }
    rmSync(root, { recursive: true, force: true });
  }
}, 60000);
