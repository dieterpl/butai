// Generate explicit file imports so Bun embeds both Vite assets and the daemon.
import { readdirSync, mkdirSync } from "node:fs";
import { resolve, join } from "node:path";

const binary = resolve(process.argv[2] ?? "../target/release/butai.exe");
const output = resolve(process.argv[3] ?? "../dist/butai-web-windows-alpha.exe");
if (!(await Bun.file(binary).exists())) throw new Error(`Missing daemon: ${binary}`);
if (!(await Bun.file("dist/index.html").exists())) throw new Error("Run bun run build first");
const files = readdirSync("dist", { recursive: true, withFileTypes: true })
  .filter((f) => f.isFile())
  .map((f) => join(f.parentPath, f.name).replaceAll("\\", "/"));
const imports = files.map((file, i) => `import asset${i} from ${JSON.stringify(resolve(file))} with { type: "file" };`);
const source = [
  'import { configureEmbedded } from "../server/static.ts";',
  'import { launch } from "../server/desktop.ts";',
  `import binary from ${JSON.stringify(binary)} with { type: "file" };`,
  ...imports,
  `configureEmbedded({${files.map((f, i) => `${JSON.stringify(f.slice(5))}: asset${i}`).join(",")}});`,
  'await launch(binary);',
].join("\n");
mkdirSync(resolve("../dist"), { recursive: true });
const entry = resolve("scripts/desktop.generated.ts");
await Bun.write(entry, source);
const build = Bun.spawn([process.execPath, "build", "--compile", "--target=bun-windows-x64", entry, "--outfile", output], { stdout: "inherit", stderr: "inherit" });
if (await build.exited !== 0) throw new Error("Windows launcher compilation failed");
console.log(`Built ${output}`);
