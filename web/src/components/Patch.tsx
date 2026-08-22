// Patch — a unified diff, coloured by where each line came from.
//
// In the kit because **more than one page renders one**: GIT shows a commit or
// a stash, FILES shows a file's own diff, and in the old client that meant two
// components carrying the same four rules between them.
//
// It owns its horizontal scroll. A diff is the widest thing this client draws —
// two versions of a line plus a marker column — and a page that scrolls
// sideways takes its rails and its header along with it, so the overflow stops
// here.

import * as React from "react";

import { cn } from "@/lib/utils";
import { CODE_BOX } from "@/components/Code";

// Which colour a line takes. **The order is load-bearing** and it is the
// terminal client's (`renderPatch`): `+++` is tested after `+`, so a file
// header reads as an addition. The two clients draw the same patch, and a `@@`
// coloured differently in one of them is visibly two products.
//
// The last arm is therefore only reachable for `diff ` and `index ` lines
// today. It stays written out because it is what makes the ordering above a
// decision rather than an accident.
function patchTone(line: string): string {
  if (line.startsWith("+")) return "text-ok";
  if (line.startsWith("-")) return "text-bad";
  if (line.startsWith("@@")) return "font-semibold text-primary";
  if (
    line.startsWith("diff ") ||
    line.startsWith("index ") ||
    line.startsWith("--- ") ||
    line.startsWith("+++ ")
  ) {
    return "text-dim";
  }
  return "";
}

type PatchProps = Omit<React.ComponentProps<"pre">, "children"> & {
  /** Raw unified-diff text, as `git diff` prints it — [`DiffDto`]'s `patch`. */
  text: string;
};

function Patch({ className, text, ...props }: PatchProps) {
  const lines = text.split("\n");
  return (
    // One `<pre>` of inline spans, each ending in its own newline, rather than
    // a block element per line: a block per line would let a row take a tinted
    // background, but copying it out depends on the browser reinserting the
    // line breaks. A diff you cannot paste into a comment is not worth a tint.
    <pre data-slot="patch" {...props} className={cn(CODE_BOX, "overflow-auto", className)}>
      {lines.map((line, i) => (
        <span key={i} className={patchTone(line) || undefined}>
          {line + "\n"}
        </span>
      ))}
    </pre>
  );
}

export { Patch, patchTone };
