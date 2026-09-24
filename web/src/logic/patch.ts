export interface PatchLine { text: string; old?: number; next?: number }
export interface PatchFile { name: string; lines: PatchLine[]; added: number; deleted: number }

/** Number each side independently; additions and deletions exist on one side. */
export function patchLines(text: string): PatchLine[] {
  let old = 0, next = 0, oldLeft = 0, nextLeft = 0;
  const lines = text.split("\n");
  if (lines.at(-1) === "") lines.pop();
  return lines.map(text => {
    const hunk = /^@@ -(\d+)(?:,(\d+))? \+(\d+)(?:,(\d+))? @@/.exec(text);
    if (hunk) {
      old = Number(hunk[1]); next = Number(hunk[3]);
      oldLeft = Number(hunk[2] ?? 1); nextLeft = Number(hunk[4] ?? 1);
      return { text };
    }
    if (text.startsWith("diff ")) { oldLeft = nextLeft = 0; return { text }; }
    if (!oldLeft && !nextLeft) return { text };
    if (text.startsWith("-")) { oldLeft--; return { text, old: old++ }; }
    if (text.startsWith("+")) { nextLeft--; return { text, next: next++ }; }
    if (text.startsWith(" ")) { oldLeft--; nextLeft--; return { text, old: old++, next: next++ }; }
    return { text };
  });
}

/** One frame per file, including renamed, removed and binary files. */
export function patchFiles(text: string): PatchFile[] {
  const files: PatchFile[] = [];
  let file: PatchFile | null = null;
  for (const line of patchLines(text)) {
    if (line.text.startsWith("diff --git ") || !file) {
      const name = /^diff --git a\/.* b\/(.+)$/.exec(line.text)?.[1] ?? /"b\/(.+)"$/.exec(line.text)?.[1] ?? "diff";
      file = { name, lines: [], added: 0, deleted: 0 };
      files.push(file);
    }
    if (line.old != null || line.next != null) {
      file.lines.push(line);
      if (line.old == null) file.added++;
      if (line.next == null) file.deleted++;
    } else if (line.text.startsWith("--- ") || line.text.startsWith("+++ ")) {
      const path = line.text.slice(4).split("\t")[0]!;
      if (path !== "/dev/null") file.name = path.replace(/^[ab]\//, "");
    } else if (line.text.startsWith("rename to ")) {
      file.name = line.text.slice(10); file.lines.push(line);
    } else if (!line.text.startsWith("diff --git ") && !line.text.startsWith("index ")) {
      file.lines.push(line);
    }
  }
  return files;
}

export interface SyntaxRun { text: string; tone: string }
/** Keep strings/comments intact so keywords inside them never change pen. */
export function syntaxRuns(text: string): SyntaxRun[] {
  const tokens = /"(?:[^"\\]|\\.)*"|'(?:[^'\\]|\\.)*'|\/\/.*$|\/\*.*?\*\/|#.*$|\b(?:as|async|await|break|case|catch|class|const|continue|def|else|enum|export|false|fn|for|from|if|impl|import|in|interface|let|match|mod|mut|new|None|null|pub|return|self|Some|static|struct|super|throw|trait|true|try|type|use|var|while|yield)\b|\b\d+(?:\.\d+)?\b/g;
  const runs: SyntaxRun[] = [];
  let at = 0;
  for (const match of text.matchAll(tokens)) {
    if (match.index > at) runs.push({ text: text.slice(at, match.index), tone: "" });
    const token = match[0];
    const tone = token.startsWith("//") || token.startsWith("/*") || token.startsWith("#") ? "text-faint" :
      token.startsWith('"') || token.startsWith("'") ? "text-ok" : /^\d/.test(token) ? "text-info" : "text-primary";
    runs.push({ text: token, tone });
    at = match.index + token.length;
  }
  if (at < text.length) runs.push({ text: text.slice(at), tone: "" });
  return runs;
}
