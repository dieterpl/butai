import { expect, test } from "bun:test";
import { brailleTrace } from "../src/logic/trace";
import { patchLines } from "../src/logic/patch";

test("terminal traces keep idle baselines and silent traffic distinct", () => {
  expect(brailleTrace([], 3)).toBe("⣀⣀⣀");
  expect(brailleTrace([], 3, true)).toBe("⠀⠀⠀");
  expect(brailleTrace([100, 100], 2)).toBe("⣀⣿");
  expect(brailleTrace([0, 100], 1, true)).toBe("⢸");
  expect(brailleTrace([100], 0)).toBe("");
});

test("diff numbering follows each side across deletions, additions and new files", () => {
  const numbered = patchLines("--- a/old\n+++ b/new\n@@ -8,2 +8,3 @@\n keep\n-old\n+new\n+extra\n\\ No newline at end of file\n@@ -0,0 +1 @@\n+created");
  expect(numbered.filter(r => r.old != null || r.next != null)).toEqual([
    { text: " keep", old: 8, next: 8 }, { text: "-old", old: 9 },
    { text: "+new", next: 9 }, { text: "+extra", next: 10 },
    { text: "+created", next: 1 },
  ]);
});

import { agentMark } from "../src/pages/parts";
import type { AgentDto } from "../src/protocol/generated/protocol";

test("agent tokens retain urgency, unread turns and the current turn's elapsed time", () => {
  const agent = { state: "waiting", exited: null, unread: false } as AgentDto;
  expect(agentMark(agent, 0).short).toBe("WAIT");
  expect(agentMark({ ...agent, state: "finished", unread: true }, 0).short).toBe("done•");
  expect(agentMark({ ...agent, state: "finished" }, 0).tone).toBe("idle");
  expect(agentMark({ ...agent, exited: 2, unread: true }, 0).short).toBe("exit 2•");
  expect(agentMark({ ...agent, state: "working", working_since_ms: 1000 }, 76000).short).toMatch(/ 1:15$/);
});

import { patchFiles, syntaxRuns } from "../src/logic/patch";

test("diff file frames distinguish deleted files and binary metadata", () => {
  const files = patchFiles("diff --git a/old.rs b/old.rs\n--- a/old.rs\n+++ /dev/null\n@@ -1 +0,0 @@\n-removed\ndiff --git a/image.png b/image.png\nBinary files differ");
  expect(files.map(f => [f.name, f.added, f.deleted])).toEqual([["old.rs", 0, 1], ["image.png", 0, 0]]);
  expect(files[1]!.lines[0]!.text).toBe("Binary files differ");
});

test("source strings and comments keep keywords inside the same token", () => {
  expect(syntaxRuns('let text = "if true"; // return 42').filter(r => r.tone)).toEqual([
    { text: "let", tone: "text-primary" }, { text: '"if true"', tone: "text-ok" },
    { text: "// return 42", tone: "text-faint" },
  ]);
});
