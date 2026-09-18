// Entry point used by the compiled, self-contained Windows web launcher.
import { createHash, randomBytes } from "node:crypto";
import { mkdirSync, existsSync, renameSync, readFileSync } from "node:fs";
import { homedir } from "node:os";
import { join } from "node:path";

export async function launch(binaryAsset: string): Promise<void> {
  if (process.argv.includes("--version")) {
    console.log("butai web launcher (Windows alpha)");
    return;
  }
  const bytes = await Bun.file(binaryAsset).bytes();
  const hash = createHash("sha256").update(bytes).digest("hex");
  const root = join(Bun.env.LOCALAPPDATA ?? homedir(), "butai", "web-runtime", hash);
  mkdirSync(root, { recursive: true });
  const binary = join(root, "butai.exe");
  if (!existsSync(binary)) {
    const temporary = `${binary}.${process.pid}.tmp`;
    await Bun.write(temporary, bytes);
    renameSync(temporary, binary);
  }
  if (createHash("sha256").update(readFileSync(binary)).digest("hex") !== hash) {
    throw new Error("The extracted butai daemon failed integrity verification");
  }
  const state = Bun.env.BUTAI_HOME ?? join(homedir(), ".butai");
  mkdirSync(state, { recursive: true });
  Bun.env.BUTAI_BIN = binary;
  Bun.env.BUTAI_SOCKET = Bun.env.BUTAI_HOME
    ? join(state, "butai.sock")
    : Bun.env.BUTAI_SOCKET ?? join(state, "butai.sock");
  Bun.env.BUTAI_WEB_DESKTOP = "1";
  Bun.env.PORT ??= "0";
  const probe = Bun.spawn([binary, "--socket", Bun.env.BUTAI_SOCKET, "ws", "ls"], {
    stdout: "ignore", stderr: "inherit", windowsHide: true,
  });
  if (await probe.exited !== 0) throw new Error("Could not start the butai daemon");
  Bun.env.BUTAI_WEB_GATEWAY_TOKEN = randomBytes(32).toString("hex");
  const gateway = Bun.spawn([binary, "--socket", Bun.env.BUTAI_SOCKET, "web-gateway"], {
    stdin: "pipe", stdout: "pipe", stderr: "inherit", windowsHide: true,
    env: { ...process.env, BUTAI_WEB_GATEWAY_TOKEN: Bun.env.BUTAI_WEB_GATEWAY_TOKEN },
  });
  const reader = gateway.stdout.getReader();
  const first = await reader.read();
  reader.releaseLock();
  const port = Number(new TextDecoder().decode(first.value).trim());
  if (!Number.isInteger(port) || port < 1 || port > 65535) throw new Error("Could not start the HTTP adapter");
  Bun.env.BUTAI_WEB_GATEWAY = `http://127.0.0.1:${port}`;
  process.on("exit", () => gateway.kill());
  await import("./index.ts");
  console.log("Keep this launcher running while using butai in your browser. Close it to stop the web bridge.");
}
