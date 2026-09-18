// Windows uses the native proxy process to reach private named-pipe IPC.
// Unix continues using Bun's existing direct socket transport.
import { spawn } from "node:child_process";
import { Duplex } from "node:stream";

export function proxyStream(socket: string): Duplex {
  const child = spawn(Bun.env.BUTAI_BIN ?? "butai.exe", ["--socket", socket, "proxy"], {
    windowsHide: true,
    stdio: ["pipe", "pipe", "ignore"],
  });
  const stream = new Duplex({
    read() { child.stdout.resume(); },
    write(chunk, encoding, callback) { child.stdin.write(chunk, encoding, callback); },
    final(callback) { child.stdin.end(callback); },
    destroy(error, callback) { child.kill(); callback(error); },
  });
  child.stdout.on("data", (chunk) => { if (!stream.push(chunk)) child.stdout.pause(); });
  child.stdout.on("end", () => stream.push(null));
  child.stdout.on("error", (error) => stream.destroy(error));
  child.stdin.on("error", (error) => stream.destroy(error));
  child.on("error", (error) => stream.destroy(error));
  child.on("exit", (code) => {
    if (code) stream.destroy(new Error(`butai proxy exited with ${code}`));
  });
  stream.on("close", () => child.kill());
  return stream;
}

export async function proxyResponse(
  socket: string, method: string, path: string, body?: Uint8Array,
  ctype = "application/json", signal?: AbortSignal,
): Promise<Response> {
  if (!Bun.env.BUTAI_WEB_GATEWAY || socket !== Bun.env.BUTAI_SOCKET) {
    throw new Error("Windows HTTP bridge requires the packaged web launcher and its configured daemon");
  }
  const headers = new Headers({ "Content-Type": ctype, "X-Butai-Web-Token": Bun.env.BUTAI_WEB_GATEWAY_TOKEN ?? "" });
  return fetch(`${Bun.env.BUTAI_WEB_GATEWAY}${path}`, {
    method, headers, ...(body ? { body: body as BodyInit } : {}), ...(signal ? { signal } : {}),
  });
}
